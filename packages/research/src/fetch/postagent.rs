//! Spawn `postagent send --anonymous --json <api_url>` and parse its response.
//!
//! URL passed as argv — never via shell. Stdout capped at 16 MiB.

use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::RawFetch;

pub const POSTAGENT_STDOUT_CAP: usize = 16 * 1024 * 1024;

pub fn binary() -> String {
    std::env::var("POSTAGENT_BIN").unwrap_or_else(|_| "postagent".to_string())
}

/// Run postagent. `api_url` is the full HTTP URL the subprocess will GET.
/// Returns RawFetch on clean exit; Err on spawn / timeout / cap-exceeded.
pub fn run(api_url: &str, timeout_ms: u64) -> Result<RawFetch, String> {
    let bin = binary();
    let start = Instant::now();
    let mut child = Command::new(&bin)
        .arg("send")
        .arg("--anonymous")
        .arg("--json")
        .arg(api_url) // argv — no shell
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("MISSING_DEPENDENCY: postagent binary '{bin}' not found on PATH (install postagent or set POSTAGENT_BIN)"),
            _ => format!("spawn postagent: {e}"),
        })?;

    // Capture stdout with size cap; poll for timeout.
    let mut stdout = child.stdout.take().ok_or_else(|| "no stdout pipe".to_string())?;
    let mut stderr = child.stderr.take().ok_or_else(|| "no stderr pipe".to_string())?;
    let deadline = start + Duration::from_millis(timeout_ms);

    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::with_capacity(4096);
        let mut tmp = [0u8; 8192];
        loop {
            match stdout.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() + n > POSTAGENT_STDOUT_CAP {
                        // mark overflow by returning Err
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

    // Poll for exit / timeout
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

    let stdout_result = stdout_handle.join().map_err(|_| "stdout thread panicked".to_string())?;
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

/// Parse postagent stdout into a lightweight structural view useful for the
/// smell test. Accepts any JSON; caller decides interpretation.
pub struct ParsedApi {
    pub status: Option<i32>,
    pub body_bytes: u64,
    pub body_non_empty: bool,
}

pub fn parse(raw: &RawFetch) -> Option<ParsedApi> {
    let v: Value = serde_json::from_slice(&raw.raw_stdout).ok()?;
    let status = v.get("status").and_then(|s| s.as_i64()).map(|n| n as i32);
    let body_bytes = raw.raw_stdout.len() as u64;
    let body_non_empty = match v.get("body") {
        Some(Value::Null) | None => !v.as_object().map(|o| o.is_empty()).unwrap_or(true),
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(_) => true,
    };
    Some(ParsedApi {
        status,
        body_bytes,
        body_non_empty,
    })
}
