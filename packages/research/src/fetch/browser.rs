//! actionbook browser 3-step subprocess sequence:
//! new-tab → wait network-idle → text [--readable] → close-tab (best-effort).
//!
//! Session and tab IDs are allocated by this module: session = "research-<slug>",
//! tab = "t-<N>" where N is the add sequence number.
//!
//! Each step emits a JSON envelope `{ok, context, data, error, meta}`.
//! We parse the text step's `context.url` + `data.value` for the smell test.

use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::RawFetch;

pub const ACTIONBOOK_STDOUT_CAP: usize = 16 * 1024 * 1024;

pub fn binary() -> String {
    std::env::var("ACTIONBOOK_BIN").unwrap_or_else(|_| "actionbook".to_string())
}

pub struct BrowserRun {
    pub raw: RawFetch,
    pub observed_url: String,
    pub body: Vec<u8>,
}

pub fn session_id_for(slug: &str) -> String {
    format!("research-{slug}")
}

pub fn tab_id_for(n: u32) -> String {
    format!("t-{n}")
}

/// Run the 3-step sequence. Shared `timeout_ms` budget across all steps.
pub fn run(
    slug: &str,
    tab_n: u32,
    url: &str,
    readable: bool,
    timeout_ms: u64,
) -> Result<BrowserRun, String> {
    let bin = binary();
    let session = session_id_for(slug);
    let tab = tab_id_for(tab_n);
    let start = Instant::now();

    // Make sure session exists (auto-start idempotent — if already up the
    // subprocess returns non-zero which we treat as benign).
    let _ = one_step(
        &bin,
        &["browser", "start", "--session", &session],
        budget_remaining(start, timeout_ms)?,
    );

    // Step 1: new-tab
    let r1 = one_step(
        &bin,
        &[
            "browser", "new-tab", url, "--session", &session, "--tab", &tab, "--json",
        ],
        budget_remaining(start, timeout_ms)?,
    )?;
    if r1.exit_code != 0 {
        return Err(format!(
            "browser new-tab exit {}; stderr: {}",
            r1.exit_code,
            String::from_utf8_lossy(&r1.raw_stderr)
        ));
    }

    // Step 2: wait network-idle
    let remaining = budget_remaining(start, timeout_ms)?;
    let r2 = one_step(
        &bin,
        &[
            "browser",
            "wait",
            "network-idle",
            "--session",
            &session,
            "--tab",
            &tab,
            "--timeout",
            &remaining.to_string(),
            "--json",
        ],
        remaining,
    )?;
    // wait-idle timeout is tolerable (per B4 lesson) — don't hard-fail here
    if r2.exit_code != 0 {
        // fall through; text step will validate observed state
    }

    // Step 3: text [--readable]
    let mut args: Vec<String> = vec![
        "browser".into(),
        "text".into(),
        "--session".into(),
        session.clone(),
        "--tab".into(),
        tab.clone(),
        "--json".into(),
    ];
    if readable {
        args.push("--readable".into());
    }
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let r3 = one_step(&bin, &arg_refs, budget_remaining(start, timeout_ms)?)?;
    if r3.exit_code != 0 {
        return Err(format!(
            "browser text exit {}; stderr: {}",
            r3.exit_code,
            String::from_utf8_lossy(&r3.raw_stderr)
        ));
    }

    // Best-effort close-tab (ignore failure).
    let _ = one_step(
        &bin,
        &[
            "browser", "close-tab", "--session", &session, "--tab", &tab, "--json",
        ],
        budget_remaining(start, timeout_ms).unwrap_or(2000),
    );

    // Parse text step's JSON envelope for observed_url + body.
    let v: Value = serde_json::from_slice(&r3.raw_stdout).map_err(|e| {
        format!(
            "actionbook browser text returned non-JSON: {e}; first 256 bytes: {}",
            String::from_utf8_lossy(&r3.raw_stdout[..r3.raw_stdout.len().min(256)])
        )
    })?;
    let observed_url = v["context"]["url"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let body = v["data"]["value"]
        .as_str()
        .unwrap_or("")
        .as_bytes()
        .to_vec();

    Ok(BrowserRun {
        raw: r3, // use text step as the "primary" raw (has envelope)
        observed_url,
        body,
    })
}

fn budget_remaining(start: Instant, total_ms: u64) -> Result<u64, String> {
    let elapsed = start.elapsed().as_millis() as u64;
    if elapsed >= total_ms {
        return Err(format!("browser budget exhausted after {elapsed}ms"));
    }
    Ok(total_ms - elapsed)
}

/// Spawn + wait a single actionbook subprocess step.
fn one_step(bin: &str, args: &[&str], timeout_ms: u64) -> Result<RawFetch, String> {
    let start = Instant::now();
    let mut child = Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!(
                "MISSING_DEPENDENCY: actionbook binary '{bin}' not found on PATH (install actionbook or set ACTIONBOOK_BIN)"
            ),
            _ => format!("spawn actionbook: {e}"),
        })?;
    let mut stdout = child.stdout.take().ok_or("no stdout pipe")?;
    let mut stderr = child.stderr.take().ok_or("no stderr pipe")?;
    let deadline = start + Duration::from_millis(timeout_ms);

    let stdout_h = std::thread::spawn(move || {
        let mut buf = Vec::with_capacity(4096);
        let mut tmp = [0u8; 8192];
        loop {
            match stdout.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() + n > ACTIONBOOK_STDOUT_CAP {
                        return Err(buf.len() as u64);
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                Err(_) => break,
            }
        }
        Ok(buf)
    });
    let stderr_h = std::thread::spawn(move || {
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
                    return Err(format!("actionbook step timed out after {timeout_ms}ms"));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("wait actionbook: {e}")),
        }
    };
    let raw_stdout = stdout_h
        .join()
        .map_err(|_| "stdout thread panicked".to_string())?
        .map_err(|_| format!("actionbook stdout exceeded {} MiB cap", ACTIONBOOK_STDOUT_CAP / (1024 * 1024)))?;
    let raw_stderr = stderr_h.join().unwrap_or_default();

    Ok(RawFetch {
        raw_stdout,
        raw_stderr,
        exit_code,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}
