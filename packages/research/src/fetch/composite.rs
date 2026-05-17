//! Composite fan-out fetch: run multiple parts sequentially, merge into ONE
//! source artifact.
//!
//! Spec: `specs/composite-source-fetch.spec.md`.
//!
//! ## Semantics
//!
//! - **Sequential** (NOT parallel — spec § 已定决策). Each part runs in fan-out
//!   order; the next part only starts after the previous one's smell verdict.
//! - **All-or-nothing smell**: ANY part rejected → the whole composite is
//!   rejected, the failing label propagates to `composite_failed_part`, and a
//!   `<label>: <reason>` warning is prefixed.
//! - **Short-circuit on first failure**: subsequent parts are NOT invoked
//!   (saves the rendered-browser cost when metadata API 404s first).
//! - **Trust = max(parts.trust)** (spec § Trust score 计算).
//! - **Raw artifact** is a single `{schema: "composite-v1", parts: {<label>: {...}}}`
//!   JSON document — keyed by label, schema-tagged for forward-compat.
//!
//! The merge logic lives here (NOT in `smell.rs`) so the smell layer stays
//! oblivious to composite semantics. Each part is judged via the existing
//! single-backend smell entry points (`judge_api` / `judge_browser_with`).
//!
//! ## Per-part timeout budget
//!
//! Spec § "composite_partial_timeout_one_part_propagates_reject" requires
//! `--timeout` to be a **per-part** budget (each part gets the full
//! `timeout_ms`), not a shared budget. This matches the single-backend
//! call convention and lets a slow first part not starve the second.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

use super::{
    browser, postagent, smell, FetchOutcome, RawFetch,
};
use crate::route::{Executor as RouteExecutor, ResolvedPart};
use crate::session::event::RejectReason;

/// Output of executing one composite part.
struct PartResult {
    label: String,
    executor: RouteExecutor,
    raw: RawFetch,
    outcome: FetchOutcome,
    trust_score: f64,
}

/// Composite entry point. Runs each `part` sequentially and merges the
/// results into a single `(raw_bytes, outcome, "composite")` triple.
///
/// `slug` / `raw_n` are forwarded to browser parts for tab-handle naming;
/// `url` is the canonical user-supplied URL (used as a fallback when a
/// part fails before producing an observed URL).
#[allow(clippy::too_many_arguments)]
pub fn execute_composite(
    parts: &[ResolvedPart],
    slug: &str,
    raw_n: u32,
    url: &str,
    readable: bool,
    timeout_ms: u64,
    smell_cfg: smell::SmellConfig,
    frame_id: Option<u32>,
    run_code_args: Option<&Value>,
) -> (Vec<u8>, FetchOutcome, String) {
    debug_assert!(parts.len() >= 2, "composite must have ≥ 2 parts (validated at load)");

    let labels: Vec<String> = parts.iter().map(|p| p.label.clone()).collect();
    let mut results: Vec<PartResult> = Vec::with_capacity(parts.len());
    let mut failed_label: Option<String> = None;
    let mut failure_outcome: Option<FetchOutcome> = None;

    for (idx, part) in parts.iter().enumerate() {
        eprintln!(
            "[composite] running part {}/{} ({})",
            idx + 1,
            parts.len(),
            part.label
        );
        let part_result = match part.executor {
            RouteExecutor::Postagent => {
                run_part_postagent(&part.label, &part.command, timeout_ms)
            }
            RouteExecutor::Browser => run_part_browser(
                &part.label,
                slug,
                raw_n,
                &part.command,
                readable,
                timeout_ms,
                smell_cfg,
                frame_id,
                run_code_args,
            ),
            RouteExecutor::Local => {
                // Local executor isn't a sensible composite part (it has no
                // template URL to substitute against). Treat as a fatal
                // composite reject rather than silently dropping the part.
                let mut outcome = FetchOutcome {
                    accepted: false,
                    observed_url: Some(url.into()),
                    observed_bytes: 0,
                    reject_reason: Some(RejectReason::FetchFailed),
                    warnings: vec![format!(
                        "composite part `{}`: local executor not supported in composite",
                        part.label
                    )],
                    bytes: 0,
                    ..Default::default()
                };
                outcome.warnings = prefix_warnings(&part.label, &outcome.warnings);
                PartResult {
                    label: part.label.clone(),
                    executor: RouteExecutor::Local,
                    raw: RawFetch {
                        raw_stdout: Vec::new(),
                        raw_stderr: Vec::new(),
                        exit_code: -1,
                        duration_ms: 0,
                    },
                    outcome,
                    trust_score: 0.0,
                }
            }
        };

        if !part_result.outcome.accepted {
            // Short-circuit: this part failed → composite reject. Subsequent
            // parts are NOT invoked.
            failed_label = Some(part_result.label.clone());
            failure_outcome = Some(part_result.outcome.clone());
            results.push(part_result);
            break;
        }
        results.push(part_result);
    }

    // ── Build merged raw artifact (always — accepted or rejected) ─────────
    let merged_json = build_composite_artifact(&results);
    let raw_bytes = merged_json.into_bytes();

    if failed_label.is_some() {
        let failed = failed_label.unwrap();
        // Take the first part's failure reason; sum already-collected bytes.
        let first_failure = failure_outcome.expect("failed_label set ⇒ outcome set");
        let total_bytes: u64 = results.iter().map(|r| r.outcome.bytes).sum();
        // Warnings: composite-level summary + each part's own warnings
        // (already label-prefixed by the per-part runner).
        let mut warnings = Vec::with_capacity(8);
        warnings.push(format!(
            "composite reject: part `{failed}` failed → entire source rejected"
        ));
        for r in &results {
            for w in &r.outcome.warnings {
                warnings.push(w.clone());
            }
        }
        let outcome = FetchOutcome {
            accepted: false,
            observed_url: first_failure.observed_url.clone(),
            observed_bytes: total_bytes,
            reject_reason: first_failure.reject_reason,
            warnings,
            bytes: total_bytes,
            composite_parts: Some(labels),
            composite_part_bytes: Some(part_bytes_map(&results)),
            composite_failed_part: Some(failed),
            composite_part_trust: Some(part_trust_map(&results)),
        };
        return (raw_bytes, outcome, "composite".into());
    }

    // All parts accepted.
    let total_bytes: u64 = results.iter().map(|r| r.outcome.bytes).sum();
    let mut warnings: Vec<String> = Vec::new();
    for r in &results {
        for w in &r.outcome.warnings {
            warnings.push(w.clone());
        }
    }
    let outcome = FetchOutcome {
        accepted: true,
        // observed_url: use the last accepted part's observed URL for the
        // single-source convention; if none of the parts surfaced one
        // (e.g. all postagent), fall back to the request URL.
        observed_url: results
            .iter()
            .rev()
            .find_map(|r| r.outcome.observed_url.clone())
            .or_else(|| Some(url.into())),
        observed_bytes: total_bytes,
        reject_reason: None,
        warnings,
        bytes: total_bytes,
        composite_parts: Some(labels),
        composite_part_bytes: Some(part_bytes_map(&results)),
        composite_failed_part: None,
        composite_part_trust: Some(part_trust_map(&results)),
    };

    (raw_bytes, outcome, "composite".into())
}

fn part_bytes_map(results: &[PartResult]) -> BTreeMap<String, u64> {
    let mut m = BTreeMap::new();
    for r in results {
        m.insert(r.label.clone(), r.outcome.bytes);
    }
    m
}

fn part_trust_map(results: &[PartResult]) -> BTreeMap<String, f64> {
    let mut m = BTreeMap::new();
    for r in results {
        m.insert(r.label.clone(), r.trust_score);
    }
    m
}

/// Emit `{schema: "composite-v1", parts: {<label>: {...}}}` JSON.
fn build_composite_artifact(results: &[PartResult]) -> String {
    let mut parts_obj = Map::new();
    for r in results {
        let mut part_obj = Map::new();
        part_obj.insert(
            "executor".into(),
            Value::String(r.executor.as_str().into()),
        );

        // Prefer UTF-8 stdout; fall back to base64 for binary payloads. The
        // spec calls out `raw_stdout_b64` as the binary-safe alternative.
        match std::str::from_utf8(&r.raw.raw_stdout) {
            Ok(s) => {
                part_obj.insert("raw_stdout_utf8".into(), Value::String(s.into()));
            }
            Err(_) => {
                part_obj.insert(
                    "raw_stdout_b64".into(),
                    Value::String(base64_encode(&r.raw.raw_stdout)),
                );
            }
        }
        part_obj.insert(
            "exit_code".into(),
            Value::Number(serde_json::Number::from(r.raw.exit_code as i64)),
        );
        part_obj.insert(
            "duration_ms".into(),
            Value::Number(serde_json::Number::from(r.raw.duration_ms)),
        );
        part_obj.insert("smell_pass".into(), Value::Bool(r.outcome.accepted));
        part_obj.insert(
            "trust_score".into(),
            json!(r.trust_score),
        );
        parts_obj.insert(r.label.clone(), Value::Object(part_obj));
    }
    let doc = json!({
        "schema": "composite-v1",
        "parts": Value::Object(parts_obj),
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".into())
}

/// Lightweight base64 encoder (RFC 4648 standard alphabet, no padding-strip).
/// Used for the rare binary-stdout case so we don't pull a base64 crate
/// just for the cold path.
fn base64_encode(data: &[u8]) -> String {
    const ALPHA: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(((data.len() + 2) / 3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.len();
        let b0 = chunk[0];
        let b1 = if n > 1 { chunk[1] } else { 0 };
        let b2 = if n > 2 { chunk[2] } else { 0 };
        let combined = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(ALPHA[((combined >> 18) & 0x3f) as usize] as char);
        out.push(ALPHA[((combined >> 12) & 0x3f) as usize] as char);
        if n > 1 {
            out.push(ALPHA[((combined >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if n > 2 {
            out.push(ALPHA[(combined & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

// ─── Per-part runners ──────────────────────────────────────────────────────

/// Postagent part: parse command into argv, run postagent, judge via
/// `smell::judge_api`. Label-prefix every warning.
fn run_part_postagent(label: &str, command: &str, timeout_ms: u64) -> PartResult {
    // Reuse the same template→argv parser as the single-backend path. If
    // parsing fails (unlikely — placeholder substitution already happened),
    // fall back to the bare `send <url>` shape.
    let args = super::postagent_args_from_template(command).unwrap_or_else(|| {
        vec![
            "send".to_string(),
            super::extract_api_url(command).unwrap_or_default(),
        ]
    });
    match postagent::run_args(&args, timeout_ms) {
        Ok(raw) => {
            let stderr_text = String::from_utf8_lossy(&raw.raw_stderr).into_owned();
            let stderr_has_warning_marker =
                stderr_text.contains('⚠') || stderr_text.contains("connection failed");
            let mut outcome = match postagent::parse(&raw) {
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
            outcome.warnings = prefix_warnings(label, &outcome.warnings);
            let trust = part_trust(RouteExecutor::Postagent, false, outcome.bytes);
            PartResult {
                label: label.into(),
                executor: RouteExecutor::Postagent,
                raw,
                outcome,
                trust_score: trust,
            }
        }
        Err(msg) => {
            let outcome = FetchOutcome {
                accepted: false,
                observed_url: None,
                observed_bytes: 0,
                reject_reason: Some(RejectReason::FetchFailed),
                warnings: prefix_warnings(label, &[msg]),
                bytes: 0,
                ..Default::default()
            };
            PartResult {
                label: label.into(),
                executor: RouteExecutor::Postagent,
                raw: RawFetch {
                    raw_stdout: Vec::new(),
                    raw_stderr: Vec::new(),
                    exit_code: -1,
                    duration_ms: 0,
                },
                outcome,
                trust_score: 0.0,
            }
        }
    }
}

/// Browser part: run via `browser::run`, judge via `judge_browser_with`.
/// The `requested_url` for the smell check is extracted from the part's
/// command string (look for the first `https://` / `http://` token).
#[allow(clippy::too_many_arguments)]
fn run_part_browser(
    label: &str,
    slug: &str,
    raw_n: u32,
    command: &str,
    readable: bool,
    timeout_ms: u64,
    smell_cfg: smell::SmellConfig,
    frame_id: Option<u32>,
    run_code_args: Option<&Value>,
) -> PartResult {
    let requested_url = extract_url_from_command(command).unwrap_or_else(String::new);
    match browser::run(
        slug,
        raw_n,
        &requested_url,
        readable,
        timeout_ms,
        frame_id,
        run_code_args,
    ) {
        Ok(run) => {
            let mut outcome = smell::judge_browser_with(
                &smell::BrowserResponse {
                    requested_url: &requested_url,
                    observed_url: &run.observed_url,
                    body_bytes: &run.body,
                    readable_mode: readable,
                },
                smell_cfg,
            );
            outcome.warnings = prefix_warnings(label, &outcome.warnings);
            let trust = part_trust(RouteExecutor::Browser, readable, outcome.bytes);
            PartResult {
                label: label.into(),
                executor: RouteExecutor::Browser,
                raw: run.raw,
                outcome,
                trust_score: trust,
            }
        }
        Err(msg) => {
            let outcome = FetchOutcome {
                accepted: false,
                observed_url: None,
                observed_bytes: 0,
                reject_reason: Some(RejectReason::FetchFailed),
                warnings: prefix_warnings(label, &[msg]),
                bytes: 0,
                ..Default::default()
            };
            PartResult {
                label: label.into(),
                executor: RouteExecutor::Browser,
                raw: RawFetch {
                    raw_stdout: Vec::new(),
                    raw_stderr: Vec::new(),
                    exit_code: -1,
                    duration_ms: 0,
                },
                outcome,
                trust_score: 0.0,
            }
        }
    }
}

/// Per-part trust scoring. Mirrors `commands::add::trust_score` but with
/// the readable-flag implicit per executor (postagent always API trust).
fn part_trust(exec: RouteExecutor, readable: bool, bytes: u64) -> f64 {
    match exec {
        RouteExecutor::Postagent => 2.0,
        RouteExecutor::Browser if readable && bytes >= 2000 => 1.5,
        RouteExecutor::Browser => 1.0,
        RouteExecutor::Local => 2.0,
    }
}

/// Prefix every warning with `<label>: ` so the composite caller can tell
/// which part raised which issue (spec § "warnings 数组前缀化 `<label>: `").
fn prefix_warnings(label: &str, warnings: &[String]) -> Vec<String> {
    warnings
        .iter()
        .map(|w| {
            if w.starts_with(&format!("{label}: ")) {
                w.clone() // avoid double-prefix
            } else {
                format!("{label}: {w}")
            }
        })
        .collect()
}

/// Find the first `https://` / `http://` token in a shell command string —
/// used to recover the requested URL for the browser smell check (the
/// smell layer needs it to compare against `observed_url`).
fn extract_url_from_command(cmd: &str) -> Option<String> {
    // Strip surrounding quotes per token; URLs in templates are often
    // wrapped in `"..."`.
    for raw_token in cmd.split_whitespace() {
        let tok = raw_token.trim_matches(|c: char| c == '"' || c == '\'');
        if tok.starts_with("https://") || tok.starts_with("http://") {
            return Some(tok.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event::RejectReason;

    fn pr(label: &str, executor: RouteExecutor, accepted: bool, trust: f64, bytes: u64) -> PartResult {
        PartResult {
            label: label.into(),
            executor,
            raw: RawFetch {
                raw_stdout: b"hello".to_vec(),
                raw_stderr: Vec::new(),
                exit_code: 0,
                duration_ms: 1,
            },
            outcome: FetchOutcome {
                accepted,
                observed_url: Some("https://example.com".into()),
                observed_bytes: bytes,
                reject_reason: if accepted { None } else { Some(RejectReason::EmptyContent) },
                warnings: Vec::new(),
                bytes,
                ..Default::default()
            },
            trust_score: trust,
        }
    }

    #[test]
    fn part_bytes_map_sums_to_total() {
        let results = vec![
            pr("metadata", RouteExecutor::Postagent, true, 2.0, 100),
            pr("rendered", RouteExecutor::Browser, true, 1.5, 200),
        ];
        let m = part_bytes_map(&results);
        assert_eq!(m["metadata"], 100);
        assert_eq!(m["rendered"], 200);
    }

    #[test]
    fn trust_max_postagent_plus_browser() {
        let results = vec![
            pr("metadata", RouteExecutor::Postagent, true, 2.0, 100),
            pr("rendered", RouteExecutor::Browser, true, 1.5, 200),
        ];
        let m = part_trust_map(&results);
        let max = m.values().copied().fold(0.0_f64, f64::max);
        assert!((max - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn composite_artifact_has_schema_and_per_part_keys() {
        let results = vec![
            pr("metadata", RouteExecutor::Postagent, true, 2.0, 5),
            pr("rendered", RouteExecutor::Browser, true, 1.5, 5),
        ];
        let json_str = build_composite_artifact(&results);
        let v: Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(v["schema"], "composite-v1");
        assert!(v["parts"]["metadata"]["executor"].as_str() == Some("postagent"));
        assert!(v["parts"]["rendered"]["executor"].as_str() == Some("browser"));
        assert_eq!(v["parts"]["metadata"]["raw_stdout_utf8"], "hello");
        assert_eq!(v["parts"]["metadata"]["smell_pass"], true);
        assert!(v["parts"]["metadata"]["trust_score"].as_f64().unwrap() >= 0.0);
    }

    #[test]
    fn prefix_warnings_no_double_prefix() {
        let w = prefix_warnings("rendered", &["already prefix".to_string()]);
        assert_eq!(w[0], "rendered: already prefix");
        let w2 = prefix_warnings("rendered", &["rendered: keep me".to_string()]);
        assert_eq!(w2[0], "rendered: keep me");
    }

    #[test]
    fn extract_url_finds_https_token() {
        let cmd = r#"postagent send "https://api.github.com/repos/o/r" -H "Auth: t""#;
        assert_eq!(
            extract_url_from_command(cmd).as_deref(),
            Some("https://api.github.com/repos/o/r")
        );
    }

    #[test]
    fn extract_url_finds_in_browser_template() {
        let cmd = r#"actionbook browser new-tab "https://github.com/o/r/pull/42" --tab <t>"#;
        assert_eq!(
            extract_url_from_command(cmd).as_deref(),
            Some("https://github.com/o/r/pull/42")
        );
    }

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }
}
