//! `research batch <url1> <url2> …` — concurrent multi-URL add.
//!
//! Pipeline:
//! 1. **Preflight (serial, under the session lock).** Read session.jsonl,
//!    classify every URL against the preset, dedupe, assign monotonically
//!    increasing raw_n indices, append all `source_attempted` events in one
//!    go. This prevents the raw-index race that a naive "run add N times in
//!    parallel" design would hit.
//! 2. **Fetch (parallel).** Spawn up to `--concurrency` worker threads, each
//!    pulls a pre-classified (url, raw_n, decision) tuple from a shared
//!    queue and calls `fetch::execute`. Subprocess spawn cost + network
//!    round-trips dominate, so this is where the real speedup lives.
//! 3. **Persist (serial).** Drain the result channel, write raw files,
//!    append `source_accepted` / `source_rejected` events, rebuild the
//!    session.md sources block once at the end.
//!
//! Per-URL results are bubbled up inside an aggregated envelope; partial
//! failure is non-fatal (exit 0 if at least one succeeds).

use chrono::Utc;
use serde_json::json;
use std::collections::VecDeque;
use std::fs;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Instant;

use crate::fetch::{self, FetchOutcome};
use crate::output::Envelope;
use crate::route::{self, Executor as RouteExecutor};
use crate::session::{
    active, config,
    event::{RejectReason, RouteDecision, SessionEvent},
    layout, log, sources_block,
};

const CMD: &str = "research batch";
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_CONCURRENCY: usize = 4;
const MAX_CONCURRENCY: usize = 16;

pub fn run(
    urls: &[String],
    slug_arg: Option<&str>,
    concurrency_arg: Option<usize>,
    timeout_ms_arg: Option<u64>,
    readable_flag: bool,
    no_readable_flag: bool,
) -> Envelope {
    if urls.is_empty() {
        return Envelope::fail(CMD, "INVALID_ARGUMENT", "no URLs provided (pass ≥ 1)");
    }

    let concurrency = concurrency_arg
        .unwrap_or(DEFAULT_CONCURRENCY)
        .max(1)
        .min(MAX_CONCURRENCY);
    let timeout_ms = timeout_ms_arg.unwrap_or(DEFAULT_TIMEOUT_MS);

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
            .with_context(json!({ "session": slug }));
    }

    let cfg = match config::read(&slug) {
        Ok(c) => c,
        Err(e) => return Envelope::fail(CMD, "IO_ERROR", format!("read session.toml: {e}")),
    };

    let compiled = match route::load_preset(Some(&cfg.preset), None) {
        Ok(p) => p,
        Err(e) => {
            return Envelope::fail(CMD, "PRESET_ERROR", e.message.clone()).with_details(json!({
                "sub_code": e.sub_code.as_str(),
            }));
        }
    };

    // ── Phase 1: preflight (serial) ────────────────────────────────────────
    let wall_start = Instant::now();
    let existing = log::read_all(&slug).unwrap_or_default();
    let mut next_index = log::next_raw_index(&existing);

    // Duplicate detection based on events already in jsonl, plus per-batch
    // dedup so passing the same URL twice in one command doesn't collide.
    let mut accepted_urls: std::collections::HashSet<String> = existing
        .iter()
        .filter_map(|e| match e {
            SessionEvent::SourceAccepted { url, .. } => Some(url.clone()),
            _ => None,
        })
        .collect();

    let mut seen_in_batch: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut jobs: Vec<Job> = Vec::with_capacity(urls.len());
    let mut pre_skipped: Vec<PerUrl> = Vec::new();

    for url in urls {
        if !seen_in_batch.insert(url.clone()) {
            pre_skipped.push(PerUrl::duplicate(url, "duplicate within batch"));
            continue;
        }
        if accepted_urls.contains(url) {
            pre_skipped.push(PerUrl::duplicate(url, "already accepted in session"));
            continue;
        }

        let classification = match route::classify(&compiled, url, false) {
            Ok(c) => c,
            Err(msg) => {
                pre_skipped.push(PerUrl::invalid(url, &msg));
                continue;
            }
        };
        let r = classification.route();
        let route_decision = RouteDecision {
            executor: r.executor.as_str().into(),
            kind: r.kind.clone(),
            command_template: r.command_template.clone(),
        };

        let raw_n = next_index;
        next_index += 1;

        // Optimistic reservation — append attempted event under the jsonl lock.
        let attempted = SessionEvent::SourceAttempted {
            timestamp: Utc::now(),
            url: url.clone(),
            route_decision: route_decision.clone(),
            note: None,
        };
        if let Err(e) = log::append(&slug, &attempted) {
            return Envelope::fail(CMD, "IO_ERROR", format!("append attempted: {e}"));
        }

        let readable = if no_readable_flag {
            false
        } else if readable_flag {
            true
        } else {
            looks_like_article(url)
        };

        // Reserve so a later URL in this same batch for the same kind+host
        // can't overwrite our raw file. Mark as accepted upfront in our
        // in-memory set to collapse rare intra-batch duplicates that slipped
        // past the set check (e.g., variant casing).
        accepted_urls.insert(url.clone());
        jobs.push(Job {
            url: url.clone(),
            decision: route_decision,
            raw_n,
            host: extract_host(url).unwrap_or_else(|| "unknown".into()),
            kind: r.kind.clone(),
            executor: r.executor,
            readable,
        });
        // classification lifetime ends here
        let _ = classification;
    }

    // ── Phase 2: fetch (parallel) ──────────────────────────────────────────
    let queue: Arc<Mutex<VecDeque<Job>>> = Arc::new(Mutex::new(jobs.clone().into()));
    let (tx, rx) = mpsc::channel::<FetchResult>();
    let mut handles = Vec::with_capacity(concurrency.min(jobs.len()));

    for _ in 0..concurrency.min(jobs.len()) {
        let q = queue.clone();
        let tx = tx.clone();
        let slug_owned = slug.clone();
        let timeout = timeout_ms;
        let h = thread::spawn(move || {
            loop {
                let next = { q.lock().unwrap().pop_front() };
                let Some(job) = next else { break };
                let fetch_start = Instant::now();
                let (raw_bytes, outcome, executor_str) = fetch::execute(
                    &job.decision,
                    &slug_owned,
                    job.raw_n,
                    &job.url,
                    job.readable,
                    timeout,
                );
                let _ = tx.send(FetchResult {
                    job,
                    raw_bytes,
                    outcome,
                    executor_str,
                    duration_ms: fetch_start.elapsed().as_millis() as u64,
                });
            }
        });
        handles.push(h);
    }
    drop(tx); // close sender so the rx loop terminates

    let raw_dir = layout::session_raw_dir(&slug);
    if let Err(e) = fs::create_dir_all(&raw_dir) {
        return Envelope::fail(CMD, "IO_ERROR", format!("create raw/: {e}"));
    }

    // ── Phase 3: persist (serial) ──────────────────────────────────────────
    let mut results: Vec<PerUrl> = pre_skipped;
    while let Ok(res) = rx.recv() {
        let FetchResult { job, raw_bytes, outcome, executor_str, duration_ms } = res;
        let base = format!("{}-{}-{}", job.raw_n, job.kind, sanitize(&job.host));

        if outcome.accepted {
            let raw_path = raw_dir.join(format!("{base}.json"));
            if let Err(e) = fs::write(&raw_path, &raw_bytes) {
                results.push(PerUrl::failed(&job.url, &format!("write raw: {e}")));
                continue;
            }
            let trust = trust_score(job.executor, job.readable, outcome.bytes);
            let accepted_ev = SessionEvent::SourceAccepted {
                timestamp: Utc::now(),
                url: job.url.clone(),
                kind: job.kind.clone(),
                executor: executor_str.clone(),
                raw_path: rel_path(&raw_path),
                bytes: outcome.bytes,
                trust_score: trust,
                note: None,
            };
            if let Err(e) = log::append(&slug, &accepted_ev) {
                results.push(PerUrl::failed(&job.url, &format!("append accepted: {e}")));
                continue;
            }
            results.push(PerUrl::accepted(
                &job.url,
                &job.kind,
                &executor_str,
                outcome.bytes,
                trust,
                duration_ms,
                rel_path(&raw_path),
            ));
        } else {
            let rejected_path = raw_dir.join(format!("{base}.rejected.json"));
            let _ = fs::write(&rejected_path, &raw_bytes);
            let reason = outcome.reject_reason.unwrap_or(RejectReason::FetchFailed);
            let rejected_ev = SessionEvent::SourceRejected {
                timestamp: Utc::now(),
                url: job.url.clone(),
                kind: job.kind.clone(),
                executor: executor_str.clone(),
                reason,
                observed_url: outcome.observed_url.clone(),
                observed_bytes: Some(outcome.observed_bytes),
                rejected_raw_path: Some(rel_path(&rejected_path)),
                note: None,
            };
            let _ = log::append(&slug, &rejected_ev);
            results.push(PerUrl::rejected(
                &job.url,
                &job.kind,
                &executor_str,
                reason_str(reason),
                duration_ms,
                &outcome.warnings,
            ));
        }
    }
    for h in handles {
        let _ = h.join();
    }

    // Single sources-block rebuild at the end.
    let all = log::read_all(&slug).unwrap_or_default();
    let _ = sources_block::rebuild(&slug, &all);

    let accepted_count = results.iter().filter(|r| r.accepted).count();
    let rejected_count = results.iter().filter(|r| !r.accepted).count();

    Envelope::ok(
        CMD,
        json!({
            "total": urls.len(),
            "concurrency": concurrency,
            "accepted_count": accepted_count,
            "rejected_count": rejected_count,
            "duration_ms": wall_start.elapsed().as_millis() as u64,
            "results": results.iter().map(|r| r.to_json()).collect::<Vec<_>>(),
        }),
    )
    .with_context(json!({ "session": slug }))
}

// ── Internal types ─────────────────────────────────────────────────────────

#[derive(Clone)]
struct Job {
    url: String,
    decision: RouteDecision,
    raw_n: u32,
    host: String,
    kind: String,
    executor: RouteExecutor,
    readable: bool,
}

struct FetchResult {
    job: Job,
    raw_bytes: Vec<u8>,
    outcome: FetchOutcome,
    executor_str: String,
    duration_ms: u64,
}

struct PerUrl {
    url: String,
    accepted: bool,
    kind: String,
    executor: String,
    bytes: u64,
    trust_score: f64,
    duration_ms: u64,
    raw_path: Option<String>,
    reject_reason: Option<String>,
    warnings: Vec<String>,
}

impl PerUrl {
    fn accepted(
        url: &str,
        kind: &str,
        executor: &str,
        bytes: u64,
        trust: f64,
        duration_ms: u64,
        raw_path: String,
    ) -> Self {
        Self {
            url: url.into(),
            accepted: true,
            kind: kind.into(),
            executor: executor.into(),
            bytes,
            trust_score: trust,
            duration_ms,
            raw_path: Some(raw_path),
            reject_reason: None,
            warnings: Vec::new(),
        }
    }
    fn rejected(
        url: &str,
        kind: &str,
        executor: &str,
        reason: &str,
        duration_ms: u64,
        warnings: &[String],
    ) -> Self {
        Self {
            url: url.into(),
            accepted: false,
            kind: kind.into(),
            executor: executor.into(),
            bytes: 0,
            trust_score: 0.0,
            duration_ms,
            raw_path: None,
            reject_reason: Some(reason.into()),
            warnings: warnings.to_vec(),
        }
    }
    fn duplicate(url: &str, note: &str) -> Self {
        Self {
            url: url.into(),
            accepted: false,
            kind: "duplicate".into(),
            executor: "n/a".into(),
            bytes: 0,
            trust_score: 0.0,
            duration_ms: 0,
            raw_path: None,
            reject_reason: Some("duplicate".into()),
            warnings: vec![note.into()],
        }
    }
    fn invalid(url: &str, note: &str) -> Self {
        Self {
            url: url.into(),
            accepted: false,
            kind: "invalid".into(),
            executor: "n/a".into(),
            bytes: 0,
            trust_score: 0.0,
            duration_ms: 0,
            raw_path: None,
            reject_reason: Some("invalid_argument".into()),
            warnings: vec![note.into()],
        }
    }
    fn failed(url: &str, note: &str) -> Self {
        Self {
            url: url.into(),
            accepted: false,
            kind: "unknown".into(),
            executor: "n/a".into(),
            bytes: 0,
            trust_score: 0.0,
            duration_ms: 0,
            raw_path: None,
            reject_reason: Some("fetch_failed".into()),
            warnings: vec![note.into()],
        }
    }
    fn to_json(&self) -> serde_json::Value {
        json!({
            "url": self.url,
            "ok": self.accepted,
            "kind": self.kind,
            "executor": self.executor,
            "bytes": self.bytes,
            "trust_score": self.trust_score,
            "duration_ms": self.duration_ms,
            "raw_path": self.raw_path,
            "reject_reason": self.reject_reason,
            "warnings": self.warnings,
        })
    }
}

// ── Helpers (duplicated from add.rs — consolidate in a future refactor) ───

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

fn rel_path(p: &std::path::Path) -> String {
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

fn trust_score(exec: RouteExecutor, readable: bool, bytes: u64) -> f64 {
    match exec {
        RouteExecutor::Postagent => 2.0,
        RouteExecutor::Browser if readable && bytes >= 2000 => 1.5,
        RouteExecutor::Browser => 1.0,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_article_detects_deep_path() {
        assert!(looks_like_article("https://example.com/a/b/c/d"));
        assert!(looks_like_article("https://blog.example.com/post/foo"));
        assert!(!looks_like_article("https://example.com/"));
    }

    #[test]
    fn extract_host_strips_scheme_and_port() {
        assert_eq!(extract_host("https://www.reddit.com/r/x").unwrap(), "www.reddit.com");
        assert_eq!(extract_host("http://localhost:8080/x").unwrap(), "localhost");
    }

    #[test]
    fn sanitize_replaces_special_chars() {
        assert_eq!(sanitize("foo/bar baz"), "foo-bar-baz");
        assert_eq!(sanitize("ok-host.com"), "ok-host.com");
    }
}
