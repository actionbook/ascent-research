use chrono::Utc;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

use crate::fetch::{self, smell::{ShortBodyMode, SmellConfig}};
use crate::output::Envelope;
use crate::route::{self, Executor as RouteExecutor};
use crate::session::{
    active, config,
    event::{RejectReason, RouteDecision, SessionEvent},
    layout, log, sources_block,
};

const CMD: &str = "research add";
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

pub fn run(
    url: &str,
    slug_arg: Option<&str>,
    timeout_ms_arg: Option<u64>,
    readable_flag: bool,
    no_readable_flag: bool,
    min_bytes_arg: Option<u64>,
    on_short_body_arg: Option<&str>,
) -> Envelope {
    let slug = match slug_arg {
        Some(s) => s.to_string(),
        None => match active::get_active() {
            Some(s) => s,
            None => {
                return Envelope::fail(
                    CMD,
                    "NO_ACTIVE_SESSION",
                    "no active session — pass --slug or run `research new` first",
                );
            }
        },
    };
    if !config::exists(&slug) {
        return Envelope::fail(CMD, "SESSION_NOT_FOUND", format!("no session '{slug}'"))
            .with_context(json!({ "session": slug, "url": url }));
    }

    let cfg = match config::read(&slug) {
        Ok(c) => c,
        Err(e) => return Envelope::fail(CMD, "IO_ERROR", format!("read session.toml: {e}")),
    };

    let timeout_ms = timeout_ms_arg
        .or_else(|| {
            std::env::var("ACTIONBOOK_RESEARCH_ADD_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(DEFAULT_TIMEOUT_MS);

    let readable = if no_readable_flag {
        false
    } else if readable_flag {
        true
    } else {
        looks_like_article(url)
    };

    let existing = log::read_all(&slug).unwrap_or_default();
    if existing.iter().any(|e| match e {
        SessionEvent::SourceAccepted { url: u, .. } => u == url,
        _ => false,
    }) {
        let ev = SessionEvent::SourceRejected {
            timestamp: Utc::now(),
            url: url.into(),
            kind: "duplicate".into(),
            executor: "n/a".into(),
            reason: RejectReason::Duplicate,
            observed_url: None,
            observed_bytes: None,
            rejected_raw_path: None,
            note: None,
        };
        let _ = log::append(&slug, &ev);
        return Envelope::fail(
            CMD,
            "SMELL_REJECTED",
            format!("URL '{url}' already accepted in this session"),
        )
        .with_context(json!({ "session": slug, "url": url }))
        .with_details(reject_details(
            &RouteDecision { executor: "n/a".into(), kind: "duplicate".into(), command_template: "".into() },
            RejectReason::Duplicate,
            0,
            None,
            None,
            &["duplicate URL in session".to_string()],
            false,
        ));
    }

    let compiled = match route::load_preset(Some(&cfg.preset), None) {
        Ok(p) => p,
        Err(e) => {
            return Envelope::fail(CMD, "PRESET_ERROR", e.message.clone()).with_details(json!({
                "sub_code": e.sub_code.as_str(),
            }));
        }
    };
    let classification = match route::classify(&compiled, url, false) {
        Ok(c) => c,
        Err(msg) => return Envelope::fail(CMD, "INVALID_ARGUMENT", msg),
    };
    let r = classification.route();
    let route_decision = RouteDecision {
        executor: r.executor.as_str().into(),
        kind: r.kind.clone(),
        command_template: r.command_template.clone(),
    };

    let raw_n = log::next_raw_index(&existing);
    let host = extract_host(url).unwrap_or_else(|| "unknown".into());

    let attempted = SessionEvent::SourceAttempted {
        timestamp: Utc::now(),
        url: url.into(),
        route_decision: route_decision.clone(),
        note: None,
    };
    if let Err(e) = log::append(&slug, &attempted) {
        return Envelope::fail(CMD, "IO_ERROR", format!("append attempted: {e}"));
    }

    let smell_cfg = match parse_smell_config(min_bytes_arg, on_short_body_arg) {
        Ok(c) => c,
        Err(e) => {
            return Envelope::fail(CMD, "INVALID_ARGUMENT", e)
                .with_context(json!({ "session": slug, "url": url }));
        }
    };

    let fetch_start = std::time::Instant::now();
    let (raw_bytes, outcome, executor_str) = fetch::execute(
        &route_decision,
        &slug,
        raw_n,
        url,
        readable,
        timeout_ms,
        smell_cfg,
    );
    let duration_ms = fetch_start.elapsed().as_millis() as u64;

    let raw_dir = layout::session_raw_dir(&slug);
    if let Err(e) = fs::create_dir_all(&raw_dir) {
        return Envelope::fail(CMD, "IO_ERROR", format!("create raw/: {e}"));
    }
    let base = format!("{raw_n}-{kind}-{host}", kind = r.kind, host = sanitize(&host));

    if outcome.accepted {
        let raw_path = raw_dir.join(format!("{base}.json"));
        if let Err(e) = fs::write(&raw_path, &raw_bytes) {
            return Envelope::fail(CMD, "IO_ERROR", format!("write raw: {e}"));
        }

        let trust = trust_score(r.executor, readable, outcome.bytes);
        let accepted_ev = SessionEvent::SourceAccepted {
            timestamp: Utc::now(),
            url: url.into(),
            kind: r.kind.clone(),
            executor: executor_str.clone(),
            raw_path: rel_path(&raw_path),
            bytes: outcome.bytes,
            trust_score: trust,
            note: None,
        };
        if let Err(e) = log::append(&slug, &accepted_ev) {
            return Envelope::fail(CMD, "IO_ERROR", format!("append accepted: {e}"));
        }

        let all = log::read_all(&slug).unwrap_or_default();
        if let Err(e) = sources_block::rebuild(&slug, &all) {
            if let sources_block::RewriteError::MarkerMissing(_) = e {
                return Envelope::fail(
                    CMD,
                    "SESSION_MD_MARKER_MISSING",
                    "session.md missing sources markers (regenerate with `research new` template)",
                )
                .with_context(json!({ "session": slug, "url": url }));
            }
            eprintln!("⚠ session.md rewrite failed");
        }

        return Envelope::ok(
            CMD,
            json!({
                "route_decision": {
                    "executor": route_decision.executor,
                    "kind": route_decision.kind,
                    "command_template": route_decision.command_template,
                },
                "fetch_success": true,
                "smell_pass": true,
                "bytes": outcome.bytes,
                "warnings": outcome.warnings,
                "raw_path": rel_path(&raw_path),
                "trust_score": trust,
                "duration_ms": duration_ms,
            }),
        )
        .with_context(json!({ "session": slug, "url": url }));
    }

    let rejected_path = raw_dir.join(format!("{base}.rejected.json"));
    let _ = fs::write(&rejected_path, &raw_bytes);

    let reason = outcome.reject_reason.unwrap_or(RejectReason::FetchFailed);
    let fetch_success = !matches!(reason, RejectReason::FetchFailed);

    let rejected_ev = SessionEvent::SourceRejected {
        timestamp: Utc::now(),
        url: url.into(),
        kind: r.kind.clone(),
        executor: executor_str.clone(),
        reason,
        observed_url: outcome.observed_url.clone(),
        observed_bytes: Some(outcome.observed_bytes),
        rejected_raw_path: Some(rel_path(&rejected_path)),
        note: None,
    };
    let _ = log::append(&slug, &rejected_ev);

    Envelope::fail(
        CMD,
        "SMELL_REJECTED",
        format!("source rejected: {}", reason_str(reason)),
    )
    .with_context(json!({ "session": slug, "url": url }))
    .with_details(reject_details(
        &route_decision,
        reason,
        outcome.bytes,
        outcome.observed_url.clone(),
        Some(rel_path(&rejected_path)),
        &outcome.warnings,
        fetch_success,
    ))
}

fn reject_details(
    decision: &RouteDecision,
    reason: RejectReason,
    bytes: u64,
    observed_url: Option<String>,
    rejected_raw_path: Option<String>,
    warnings: &[String],
    fetch_success: bool,
) -> Value {
    json!({
        "route_decision": {
            "executor": decision.executor,
            "kind": decision.kind,
            "command_template": decision.command_template,
        },
        "fetch_success": fetch_success,
        "smell_pass": false,
        "bytes": bytes,
        "warnings": warnings,
        "reject_reason": reason_str(reason),
        "observed_url": observed_url,
        "rejected_raw_path": rejected_raw_path,
    })
}

fn reason_str(r: RejectReason) -> &'static str {
    match r {
        RejectReason::FetchFailed => "fetch_failed",
        RejectReason::WrongUrl => "wrong_url",
        RejectReason::EmptyContent => "empty_content",
        RejectReason::ApiError => "api_error",
        RejectReason::Duplicate => "duplicate",
    }
}

fn looks_like_article(url: &str) -> bool {
    let l = url.to_lowercase();
    ["/blog/", "/post/", "/rfd/", "/paper/", "/article/"]
        .iter()
        .any(|s| l.contains(s))
        || url.split('/').filter(|s| !s.is_empty()).count() >= 4
}

fn extract_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split('/').next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    Some(host.split(':').next()?.to_ascii_lowercase())
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '-' })
        .collect()
}

fn rel_path(p: &Path) -> String {
    let comps: Vec<_> = p.components().collect();
    let n = comps.len();
    if n >= 2 {
        format!(
            "{}/{}",
            comps[n - 2].as_os_str().to_string_lossy(),
            comps[n - 1].as_os_str().to_string_lossy()
        )
    } else {
        p.to_string_lossy().into_owned()
    }
}

/// Parse `--min-bytes` + `--on-short-body` into a SmellConfig. Shared by
/// `add` and `batch` — keep the validation logic in one place.
pub(crate) fn parse_smell_config(
    min_bytes: Option<u64>,
    on_short_body: Option<&str>,
) -> Result<SmellConfig, String> {
    let short_body_mode = match on_short_body {
        None | Some("reject") => ShortBodyMode::Reject,
        Some("warn") => ShortBodyMode::Warn,
        Some(other) => {
            return Err(format!(
                "invalid --on-short-body value '{other}': expected 'warn' or 'reject'"
            ));
        }
    };
    Ok(SmellConfig {
        min_bytes_override: min_bytes,
        short_body_mode,
    })
}

fn trust_score(exec: RouteExecutor, readable: bool, bytes: u64) -> f64 {
    match exec {
        RouteExecutor::Postagent => 2.0,
        RouteExecutor::Browser if readable && bytes >= 2000 => 1.5,
        RouteExecutor::Browser => 1.0,
    }
}
