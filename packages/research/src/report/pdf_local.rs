//! Local HTML → PDF conversion through a safe headless Chromium binary.
//!
//! This is the default PDF backend because it is free and keeps report HTML
//! on the user's machine. It intentionally prefers Playwright's isolated
//! Chromium/headless_shell over the user's desktop Chrome. Desktop Chrome is
//! only used when the operator opts in with `ASR_PDF_ALLOW_SYSTEM_CHROME=1`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone)]
pub struct LocalPdfOptions {
    pub output_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct LocalPdfResult {
    pub output_path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub enum LocalPdfError {
    InputMissing(PathBuf),
    BrowserUnavailable(String),
    Io(String),
    BrowserFailed { status: Option<i32>, stderr: String },
    BrowserTimedOut { secs: u64 },
    OutputMissing(PathBuf),
}

impl std::fmt::Display for LocalPdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocalPdfError::InputMissing(path) => {
                write!(f, "input html not found: {}", path.display())
            }
            LocalPdfError::BrowserUnavailable(message) => write!(f, "{message}"),
            LocalPdfError::Io(message) => write!(f, "{message}"),
            LocalPdfError::BrowserFailed { status, stderr } => write!(
                f,
                "local chrome pdf render failed status={} stderr={}",
                status
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "signal".to_string()),
                stderr.trim()
            ),
            LocalPdfError::BrowserTimedOut { secs } => {
                write!(f, "local chrome pdf render timed out after {secs}s")
            }
            LocalPdfError::OutputMissing(path) => {
                write!(
                    f,
                    "local chrome completed but did not create {}",
                    path.display()
                )
            }
        }
    }
}

pub fn convert_html_file(
    html_path: &Path,
    options: &LocalPdfOptions,
) -> Result<LocalPdfResult, LocalPdfError> {
    if !html_path.exists() {
        return Err(LocalPdfError::InputMissing(html_path.to_path_buf()));
    }
    let html_abs = html_path
        .canonicalize()
        .map_err(|e| LocalPdfError::Io(format!("canonicalize {}: {e}", html_path.display())))?;

    if let Some(parent) = options.output_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            LocalPdfError::Io(format!("create pdf output dir {}: {e}", parent.display()))
        })?;
    }

    let browser = find_browser()
        .ok_or_else(|| LocalPdfError::BrowserUnavailable(browser_unavailable_message()))?;
    let profile_dir = temp_profile_dir();
    fs::create_dir_all(&profile_dir).map_err(|e| {
        LocalPdfError::Io(format!(
            "create chrome temp profile {}: {e}",
            profile_dir.display()
        ))
    })?;

    let child = Command::new(&browser)
        .arg("--headless=new")
        .arg("--disable-gpu")
        .arg("--disable-dev-shm-usage")
        .arg("--no-sandbox")
        .arg("--disable-background-networking")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-extensions")
        .arg("--hide-scrollbars")
        .arg("--allow-file-access-from-files")
        .arg("--run-all-compositor-stages-before-draw")
        .arg("--virtual-time-budget=10000")
        .arg("--print-to-pdf-no-header")
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .arg(format!("--print-to-pdf={}", options.output_path.display()))
        .arg(file_url(&html_abs))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| LocalPdfError::BrowserUnavailable(format!("spawn `{browser}`: {e}")));

    let mut child = child?;
    let timeout_secs = timeout_secs();
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let status = loop {
        match child
            .try_wait()
            .map_err(|e| LocalPdfError::Io(format!("wait for `{browser}`: {e}")))?
        {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_dir_all(&profile_dir);
                return Err(LocalPdfError::BrowserTimedOut { secs: timeout_secs });
            }
            None => thread::sleep(Duration::from_millis(100)),
        }
    };

    let _ = fs::remove_dir_all(&profile_dir);

    if !status.success() {
        return Err(LocalPdfError::BrowserFailed {
            status: status.code(),
            stderr: String::new(),
        });
    }

    let metadata = fs::metadata(&options.output_path)
        .map_err(|_| LocalPdfError::OutputMissing(options.output_path.clone()))?;
    Ok(LocalPdfResult {
        output_path: options.output_path.clone(),
        bytes: metadata.len(),
    })
}

fn timeout_secs() -> u64 {
    std::env::var("ASR_PDF_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
}

#[cfg(target_os = "macos")]
fn find_browser() -> Option<String> {
    if let Ok(path) = std::env::var("ASR_PDF_CHROME_BIN")
        && !path.trim().is_empty()
    {
        return Some(path);
    }
    if let Ok(path) = std::env::var("CHROME_BIN")
        && !path.trim().is_empty()
    {
        return Some(path);
    }

    if let Some(path) = find_playwright_browser() {
        return Some(path);
    }

    for path in ["/Applications/Chromium.app/Contents/MacOS/Chromium"] {
        if Path::new(path).exists() {
            return Some(path.to_string());
        }
    }

    for name in ["chromium", "chromium-browser", "chrome", "msedge"] {
        if command_exists(name) {
            return Some(name.to_string());
        }
    }

    // Avoid launching the user's normal desktop Chrome by default. It has
    // caused GUI crash reports on macOS even with a temporary profile.
    // Operators who explicitly want this fallback can opt in.
    if std::env::var("ASR_PDF_ALLOW_SYSTEM_CHROME").as_deref() == Ok("1") {
        for path in [
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ] {
            if Path::new(path).exists() {
                return Some(path.to_string());
            }
        }
    }

    if std::env::var("ASR_PDF_ALLOW_SYSTEM_CHROME").as_deref() == Ok("1")
        && command_exists("google-chrome")
    {
        return Some("google-chrome".to_string());
    }

    None
}

#[cfg(target_os = "macos")]
fn find_playwright_browser() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let root = Path::new(&home).join("Library/Caches/ms-playwright");
    let entries = fs::read_dir(root).ok()?;
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        candidates.push(dir.join("chrome-mac/headless_shell"));
        candidates.push(dir.join("chrome-mac/Chromium.app/Contents/MacOS/Chromium"));
    }
    candidates.sort();
    candidates.reverse();
    for path in candidates {
        if path.exists() {
            return Some(path.display().to_string());
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn find_playwright_browser() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let root = Path::new(&home).join(".cache/ms-playwright");
    let entries = fs::read_dir(root).ok()?;
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        candidates.push(dir.join("chrome-linux/headless_shell"));
        candidates.push(dir.join("chrome-linux/chrome"));
    }
    candidates.sort();
    candidates.reverse();
    for path in candidates {
        if path.exists() {
            return Some(path.display().to_string());
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn find_browser() -> Option<String> {
    if let Ok(path) = std::env::var("ASR_PDF_CHROME_BIN")
        && !path.trim().is_empty()
    {
        return Some(path);
    }
    if let Ok(path) = std::env::var("CHROME_BIN")
        && !path.trim().is_empty()
    {
        return Some(path);
    }
    if let Some(path) = find_playwright_browser() {
        return Some(path);
    }
    for name in ["chromium", "chromium-browser", "chrome", "msedge"] {
        if command_exists(name) {
            return Some(name.to_string());
        }
    }
    None
}

fn command_exists(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn browser_unavailable_message() -> String {
    "no safe headless Chromium binary found; install Playwright Chromium (`npx playwright install chromium`), set ASR_PDF_CHROME_BIN, or set ASR_PDF_ALLOW_SYSTEM_CHROME=1 to opt into desktop Chrome".to_string()
}

fn temp_profile_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "ascent-research-chrome-profile-{}-{nanos}",
        std::process::id()
    ))
}

fn file_url(path: &Path) -> String {
    let mut out = String::from("file://");
    out.push_str(&percent_encode_path(&path.display().to_string()));
    out
}

fn percent_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b' ' => out.push_str("%20"),
            b'#' => out.push_str("%23"),
            b'?' => out.push_str("%3F"),
            b'%' => out.push_str("%25"),
            b'"' => out.push_str("%22"),
            _ => out.push(b as char),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_url_escapes_common_path_chars() {
        let url = file_url(Path::new("/tmp/a b/report#1?.html"));
        assert_eq!(url, "file:///tmp/a%20b/report%231%3F.html");
    }
}
