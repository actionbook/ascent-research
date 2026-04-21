//! Fetch layer: subprocess wrappers + smell test + result types.
//!
//! All external I/O lives here. Command handlers (`commands::add`)
//! orchestrate, but never spawn subprocess or parse response JSON directly.

pub mod browser;
pub mod postagent;
pub mod smell;

use serde::Serialize;

use crate::route::Executor as RouteExecutor;
use crate::session::event::{RejectReason, RouteDecision};

/// Raw output captured from a fetch subprocess (postagent or actionbook).
#[derive(Debug, Clone)]
pub struct RawFetch {
    /// The exact bytes the subprocess wrote to stdout (may be decoded JSON).
    pub raw_stdout: Vec<u8>,
    /// Subprocess stderr (saved for .rejected.json debug).
    pub raw_stderr: Vec<u8>,
    /// Exit code (0 on clean exit).
    pub exit_code: i32,
    /// Wall-clock duration.
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FetchOutcome {
    pub accepted: bool,
    pub observed_url: Option<String>,
    pub observed_bytes: u64,
    pub reject_reason: Option<RejectReason>,
    pub warnings: Vec<String>,
    /// Body length as reported / derived, for envelope `bytes` field.
    pub bytes: u64,
}

/// Execute a fetch for a single URL and return the raw bytes plus the
/// smell-tested outcome. No session state is mutated — this function is
/// side-effect-free apart from the subprocess it spawns, so it can be called
/// from a worker thread in a batch.
pub fn execute(
    decision: &RouteDecision,
    slug: &str,
    raw_n: u32,
    url: &str,
    readable: bool,
    timeout_ms: u64,
    smell_cfg: smell::SmellConfig,
) -> (Vec<u8>, FetchOutcome, String) {
    match RouteExecutor::parse(&decision.executor) {
        Some(RouteExecutor::Postagent) => run_postagent(decision, timeout_ms),
        Some(RouteExecutor::Browser) => run_browser(slug, raw_n, url, readable, timeout_ms, smell_cfg),
        None => (
            Vec::new(),
            FetchOutcome {
                accepted: false,
                observed_url: None,
                observed_bytes: 0,
                reject_reason: Some(RejectReason::FetchFailed),
                warnings: vec![format!("unknown executor '{}'", decision.executor)],
                bytes: 0,
            },
            decision.executor.clone(),
        ),
    }
}

fn run_postagent(
    decision: &RouteDecision,
    timeout_ms: u64,
) -> (Vec<u8>, FetchOutcome, String) {
    let api_url = extract_api_url(&decision.command_template).unwrap_or_default();
    match postagent::run(&api_url, timeout_ms) {
        Ok(raw) => {
            let stderr_text = String::from_utf8_lossy(&raw.raw_stderr).into_owned();
            let stderr_has_warning_marker =
                stderr_text.contains('⚠') || stderr_text.contains("connection failed");
            let outcome = match postagent::parse(&raw) {
                Some(p) => {
                    if p.status.is_none() {
                        let first = stderr_text
                            .lines()
                            .next()
                            .unwrap_or("postagent network failure")
                            .to_string();
                        FetchOutcome {
                            accepted: false,
                            observed_url: None,
                            observed_bytes: 0,
                            reject_reason: Some(RejectReason::FetchFailed),
                            warnings: vec![first],
                            bytes: 0,
                        }
                    } else if raw.exit_code != 0 && !stderr_has_warning_marker {
                        FetchOutcome {
                            accepted: false,
                            observed_url: None,
                            observed_bytes: raw.raw_stdout.len() as u64,
                            reject_reason: Some(RejectReason::FetchFailed),
                            warnings: vec![format!(
                                "postagent exit {} without expected pattern; stderr: {}",
                                raw.exit_code,
                                stderr_text.lines().next().unwrap_or("")
                            )],
                            bytes: raw.raw_stdout.len() as u64,
                        }
                    } else {
                        smell::judge_api(&smell::ApiResponse {
                            status: p.status,
                            body_non_empty: p.body_non_empty,
                            body_bytes: p.body_bytes,
                        })
                    }
                }
                None => FetchOutcome {
                    accepted: false,
                    observed_url: None,
                    observed_bytes: raw.raw_stdout.len() as u64,
                    reject_reason: Some(RejectReason::FetchFailed),
                    warnings: vec![format!("postagent output unparseable (exit {})", raw.exit_code)],
                    bytes: raw.raw_stdout.len() as u64,
                },
            };
            (raw.raw_stdout, outcome, "postagent".into())
        }
        Err(msg) => {
            let outcome = FetchOutcome {
                accepted: false,
                observed_url: None,
                observed_bytes: 0,
                reject_reason: Some(RejectReason::FetchFailed),
                warnings: vec![msg],
                bytes: 0,
            };
            (Vec::new(), outcome, "postagent".into())
        }
    }
}

fn run_browser(
    slug: &str,
    tab_n: u32,
    url: &str,
    readable: bool,
    timeout_ms: u64,
    smell_cfg: smell::SmellConfig,
) -> (Vec<u8>, FetchOutcome, String) {
    match browser::run(slug, tab_n, url, readable, timeout_ms) {
        Ok(run) => {
            let outcome = smell::judge_browser_with(
                &smell::BrowserResponse {
                    requested_url: url,
                    observed_url: &run.observed_url,
                    body_bytes: &run.body,
                    readable_mode: readable,
                },
                smell_cfg,
            );
            (run.raw.raw_stdout, outcome, "browser".into())
        }
        Err(msg) => {
            let outcome = FetchOutcome {
                accepted: false,
                observed_url: None,
                observed_bytes: 0,
                reject_reason: Some(RejectReason::FetchFailed),
                warnings: vec![msg],
                bytes: 0,
            };
            (Vec::new(), outcome, "browser".into())
        }
    }
}

fn extract_api_url(template: &str) -> Option<String> {
    let start = template.find('"')?;
    let rest = &template[start + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}
