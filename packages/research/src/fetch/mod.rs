//! Fetch layer: subprocess wrappers + smell test + result types.
//!
//! All external I/O lives here. Command handlers (`commands::add`)
//! orchestrate, but never spawn subprocess or parse response JSON directly.

pub mod browser;
pub mod browser_v2;
pub mod composite;
pub mod local;
pub mod postagent;
pub mod smell;

use serde::Serialize;

use crate::route::{Executor as RouteExecutor, ResolvedPart};
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
    /// Composite-only: list of all part labels in fan-out order. `None` on
    /// single-backend fetches.
    pub composite_parts: Option<Vec<String>>,
    /// Composite-only: per-part body byte counts. Sum equals `bytes`.
    pub composite_part_bytes: Option<std::collections::BTreeMap<String, u64>>,
    /// Composite-only: when rejected, the label of the failing part.
    pub composite_failed_part: Option<String>,
    /// Composite-only: per-part trust scores (max becomes composite trust).
    pub composite_part_trust: Option<std::collections::BTreeMap<String, f64>>,
}

impl Default for FetchOutcome {
    fn default() -> Self {
        Self {
            accepted: false,
            observed_url: None,
            observed_bytes: 0,
            reject_reason: None,
            warnings: Vec::new(),
            bytes: 0,
            composite_parts: None,
            composite_part_bytes: None,
            composite_failed_part: None,
            composite_part_trust: None,
        }
    }
}

/// Execute a fetch for a single URL and return the raw bytes plus the
/// smell-tested outcome. No session state is mutated — this function is
/// side-effect-free apart from the subprocess it spawns, so it can be called
/// from a worker thread in a batch.
///
/// `frame_id` / `run_code_args` are V2-browser-only pass-through flags
/// (spec `v2-frame-id-runcode-args.spec.md`). Non-browser executors
/// (postagent / local) silently ignore both: they have no equivalent
/// concept and the CLI marks the flags V2-only, so a quiet drop preserves
/// the simple call sites.
#[allow(clippy::too_many_arguments)]
pub fn execute(
    decision: &RouteDecision,
    slug: &str,
    raw_n: u32,
    url: &str,
    readable: bool,
    timeout_ms: u64,
    smell_cfg: smell::SmellConfig,
    frame_id: Option<u32>,
    run_code_args: Option<&serde_json::Value>,
) -> (Vec<u8>, FetchOutcome, String) {
    execute_with_composite(
        decision,
        None,
        slug,
        raw_n,
        url,
        readable,
        timeout_ms,
        smell_cfg,
        frame_id,
        run_code_args,
    )
}

/// Extended entry that accepts an optional composite-parts list (carried
/// from `route::classify` to here). When `composite_parts.is_some()` the
/// dispatch goes to `composite::execute_composite`; otherwise it falls
/// through to the single-backend path (byte-for-byte unchanged for the
/// 99% non-composite case).
#[allow(clippy::too_many_arguments)]
pub fn execute_with_composite(
    decision: &RouteDecision,
    composite_parts: Option<&[ResolvedPart]>,
    slug: &str,
    raw_n: u32,
    url: &str,
    readable: bool,
    timeout_ms: u64,
    smell_cfg: smell::SmellConfig,
    frame_id: Option<u32>,
    run_code_args: Option<&serde_json::Value>,
) -> (Vec<u8>, FetchOutcome, String) {
    if let Some(parts) = composite_parts {
        return composite::execute_composite(
            parts,
            slug,
            raw_n,
            url,
            readable,
            timeout_ms,
            smell_cfg,
            frame_id,
            run_code_args,
        );
    }
    match RouteExecutor::parse(&decision.executor) {
        Some(RouteExecutor::Postagent) => run_postagent(decision, timeout_ms),
        Some(RouteExecutor::Browser) => run_browser(
            slug,
            raw_n,
            url,
            readable,
            timeout_ms,
            smell_cfg,
            frame_id,
            run_code_args,
        ),
        Some(RouteExecutor::Local) => run_local(url, smell_cfg),
        None => (
            Vec::new(),
            FetchOutcome {
                accepted: false,
                observed_url: None,
                observed_bytes: 0,
                reject_reason: Some(RejectReason::FetchFailed),
                warnings: vec![format!("unknown executor '{}'", decision.executor)],
                bytes: 0,
                ..Default::default()
            },
            decision.executor.clone(),
        ),
    }
}

fn run_local(url: &str, smell_cfg: smell::SmellConfig) -> (Vec<u8>, FetchOutcome, String) {
    // `url` is the canonical `file:///abs/path` form produced by
    // `route::classify_as_local`. Strip the scheme to get the disk path.
    let path_str = url.strip_prefix("file://").unwrap_or(url);
    let path = std::path::Path::new(path_str);
    // Intentionally uses the fetch-stage backstop, not the walk-stage
    // default — see `local::FETCH_STAGE_BACKSTOP_BYTES` for the
    // separation-of-concerns rationale. The --max-file-bytes flag is
    // already enforced by `add_local::run` at the walk level.
    match local::read_file(path, local::FETCH_STAGE_BACKSTOP_BYTES) {
        Ok(read) => {
            // Route through the existing browser-shape smell test — it's
            // the text-content judge we already trust. observed_url is
            // the same as requested (local reads don't redirect).
            let outcome = smell::judge_browser_with(
                &smell::BrowserResponse {
                    requested_url: url,
                    observed_url: url,
                    body_bytes: &read.body,
                    readable_mode: false,
                },
                smell_cfg,
            );
            (read.body, outcome, "local".into())
        }
        Err(e) => {
            let (reason, msg) = match &e {
                local::LocalError::TooLarge { .. } => (RejectReason::FetchFailed, e.to_string()),
                local::LocalError::IsDirectory => (RejectReason::FetchFailed, e.to_string()),
                local::LocalError::NotReadable(_) => (RejectReason::FetchFailed, e.to_string()),
                local::LocalError::Binary(_) => (RejectReason::EmptyContent, e.to_string()),
            };
            let outcome = FetchOutcome {
                accepted: false,
                observed_url: Some(url.into()),
                observed_bytes: 0,
                reject_reason: Some(reason),
                warnings: vec![msg],
                bytes: 0,
                ..Default::default()
            };
            (Vec::new(), outcome, "local".into())
        }
    }
}

fn run_postagent(decision: &RouteDecision, timeout_ms: u64) -> (Vec<u8>, FetchOutcome, String) {
    let args = postagent_args_from_template(&decision.command_template).unwrap_or_else(|| {
        vec![
            "send".to_string(),
            extract_api_url(&decision.command_template).unwrap_or_default(),
        ]
    });
    match postagent::run_args(&args, timeout_ms) {
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
                            ..Default::default()
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
                            ..Default::default()
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
                    warnings: vec![format!(
                        "postagent output unparseable (exit {})",
                        raw.exit_code
                    )],
                    bytes: raw.raw_stdout.len() as u64,
                    ..Default::default()
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
                ..Default::default()
            };
            (Vec::new(), outcome, "postagent".into())
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_browser(
    slug: &str,
    tab_n: u32,
    url: &str,
    readable: bool,
    timeout_ms: u64,
    smell_cfg: smell::SmellConfig,
    frame_id: Option<u32>,
    run_code_args: Option<&serde_json::Value>,
) -> (Vec<u8>, FetchOutcome, String) {
    match browser::run(slug, tab_n, url, readable, timeout_ms, frame_id, run_code_args) {
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
                ..Default::default()
            };
            (Vec::new(), outcome, "browser".into())
        }
    }
}

pub(super) fn postagent_args_from_template(template: &str) -> Option<Vec<String>> {
    let tokens = shell_words(template)?;
    if tokens.first().map(String::as_str) != Some("postagent") {
        return None;
    }
    if tokens.len() < 2 {
        return None;
    }
    Some(tokens[1..].to_vec())
}

fn shell_words(input: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                while matches!(chars.peek(), Some(next) if next.is_whitespace()) {
                    chars.next();
                }
            }
            None => current.push(ch),
        }
    }
    if quote.is_some() {
        return None;
    }
    if !current.is_empty() {
        out.push(current);
    }
    Some(out)
}

pub(super) fn extract_api_url(template: &str) -> Option<String> {
    let start = template.find('"')?;
    let rest = &template[start + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_postagent_template_with_header_placeholder() {
        let args = postagent_args_from_template(
            r#"postagent send "https://api.github.com/repos/o/r" -H "Authorization: Bearer $POSTAGENT.GITHUB.TOKEN""#,
        )
        .unwrap();
        assert_eq!(args[0], "send");
        assert_eq!(args[1], "https://api.github.com/repos/o/r");
        assert_eq!(args[2], "-H");
        assert_eq!(args[3], "Authorization: Bearer $POSTAGENT.GITHUB.TOKEN");
    }
}
