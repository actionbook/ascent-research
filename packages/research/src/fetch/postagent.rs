//! Spawn `postagent send <api_url>` and interpret its output.
//!
//! Real postagent contract (verified L1 contract smoke, 2026-04-19):
//! - No `--json` flag — postagent's stdout IS the HTTP response body.
//! - Exit code is always 0 — success/failure must be deduced from the
//!   stdout/stderr split.
//! - Success: stdout = raw body, stderr empty.
//! - HTTP 4xx/5xx: stdout empty, stderr contains line like
//!   `⚠ 404 — endpoint does not exist at <url>`
//!   followed by `HTTP <code> <phrase>` and the response body echoed.
//! - Network failure (DNS, connection refused): stderr has
//!   `⚠ connection failed — DNS lookup or connect refused for <url>`
//!   and stdout is empty.
//!
//! URL passed as argv — never via shell. Stdout capped at 16 MiB.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::RawFetch;

pub const POSTAGENT_STDOUT_CAP: usize = 16 * 1024 * 1024;

pub fn binary() -> String {
    std::env::var("POSTAGENT_BIN").unwrap_or_else(|_| "postagent".to_string())
}

/// Run postagent. `api_url` is the full HTTP URL the subprocess will GET.
/// Returns RawFetch; caller inspects stdout/stderr to determine success.
pub fn run(api_url: &str, timeout_ms: u64) -> Result<RawFetch, String> {
    run_args(&["send".to_string(), api_url.to_string()], timeout_ms)
}

/// Run postagent with explicit argv after the binary name, e.g.
/// `["send", "https://api.github.com/...", "-H", "Authorization: ..."]`.
/// This is needed for token-bearing API hands; routing templates are parsed
/// by the fetch layer and passed here without invoking a shell.
pub fn run_args(args: &[String], timeout_ms: u64) -> Result<RawFetch, String> {
    let bin = binary();
    let start = Instant::now();
    let mut child = Command::new(&bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!(
                "MISSING_DEPENDENCY: postagent binary '{bin}' not found on PATH (install postagent or set POSTAGENT_BIN)"
            ),
            _ => format!("spawn postagent: {e}"),
        })?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "no stdout pipe".to_string())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "no stderr pipe".to_string())?;
    let deadline = start + Duration::from_millis(timeout_ms);

    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::with_capacity(4096);
        let mut tmp = [0u8; 8192];
        loop {
            match stdout.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() + n > POSTAGENT_STDOUT_CAP {
                        return Err(buf.len() as u64);
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                Err(_) => break,
            }
        }
        Ok(buf)
    });

    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::with_capacity(1024);
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let exit_code = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.code().unwrap_or(-1),
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("timeout after {timeout_ms}ms"));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("wait postagent: {e}")),
        }
    };

    let stdout_result = stdout_handle
        .join()
        .map_err(|_| "stdout thread panicked".to_string())?;
    let raw_stdout = stdout_result.map_err(|_| {
        format!(
            "postagent stdout exceeded {} MiB cap",
            POSTAGENT_STDOUT_CAP / (1024 * 1024)
        )
    })?;
    let raw_stderr = stderr_handle.join().unwrap_or_default();

    Ok(RawFetch {
        raw_stdout,
        raw_stderr,
        exit_code,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

/// Parse postagent stdout/stderr into a structural view for the smell test.
///
/// Logic: postagent always exits 0, so we deduce success from output:
/// - stdout non-empty + no `⚠` in stderr → success (status 200)
/// - stderr matches `⚠ <digits> —` → extract status (e.g. 404)
/// - stderr matches `⚠ connection failed` → status = None (network error)
pub struct ParsedApi {
    pub status: Option<i32>,
    pub body_bytes: u64,
    pub body_non_empty: bool,
}

pub fn parse(raw: &RawFetch) -> Option<ParsedApi> {
    let stderr = String::from_utf8_lossy(&raw.raw_stderr);
    let stdout_len = raw.raw_stdout.len() as u64;
    let stdout_trimmed_non_empty = !raw.raw_stdout.iter().all(|b| b.is_ascii_whitespace());

    // HTTP error pattern: ⚠ <status> —
    if let Some(status) = extract_http_status(&stderr) {
        return Some(ParsedApi {
            status: Some(status),
            body_bytes: stdout_len,
            body_non_empty: stdout_trimmed_non_empty,
        });
    }

    // Network error pattern: ⚠ connection failed
    if stderr.contains("connection failed") || stderr.contains("DNS lookup") {
        return Some(ParsedApi {
            status: None,
            body_bytes: 0,
            body_non_empty: false,
        });
    }

    // Default: success if stdout has content.
    Some(ParsedApi {
        status: Some(200),
        body_bytes: stdout_len,
        body_non_empty: stdout_trimmed_non_empty,
    })
}

/// Extract an HTTP status code from stderr like `⚠ 404 — ...`.
fn extract_http_status(stderr: &str) -> Option<i32> {
    for line in stderr.lines() {
        let t = line.trim();
        // strip the warning glyph prefix
        let rest = t.strip_prefix("⚠").or_else(|| t.strip_prefix("⚠ "))?.trim();
        // first token should be the status code
        let first_word = rest.split_whitespace().next()?;
        if let Ok(n) = first_word.parse::<i32>()
            && (100..600).contains(&n)
        {
            return Some(n);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(stdout: &[u8], stderr: &[u8]) -> RawFetch {
        RawFetch {
            raw_stdout: stdout.to_vec(),
            raw_stderr: stderr.to_vec(),
            exit_code: 0,
            duration_ms: 1,
        }
    }

    #[test]
    fn parse_success() {
        let p = parse(&mk(b"[1,2,3]", b"")).unwrap();
        assert_eq!(p.status, Some(200));
        assert!(p.body_non_empty);
    }

    #[test]
    fn parse_404_from_stderr() {
        let stderr = "⚠ 404 — endpoint does not exist at https://x/y\nHTTP 404 Not Found\n";
        let p = parse(&mk(b"", stderr.as_bytes())).unwrap();
        assert_eq!(p.status, Some(404));
        assert!(!p.body_non_empty);
    }

    #[test]
    fn parse_network_failure() {
        let stderr = "⚠ connection failed — DNS lookup or connect refused for https://x.invalid/\n";
        let p = parse(&mk(b"", stderr.as_bytes())).unwrap();
        assert_eq!(p.status, None);
        assert!(!p.body_non_empty);
    }

    #[test]
    fn parse_success_ignores_empty_lines_in_stderr() {
        let p = parse(&mk(b"{\"ok\":true}", b"\n")).unwrap();
        assert_eq!(p.status, Some(200));
    }

    #[test]
    fn extract_http_status_matches_warning_lines() {
        assert_eq!(extract_http_status("⚠ 500 — server error"), Some(500));
        assert_eq!(extract_http_status("⚠ connection failed — refused"), None);
    }
}
