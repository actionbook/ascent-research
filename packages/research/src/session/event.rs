//! Canonical `SessionEvent` schema — the single source of truth referenced
//! by foundation spec. 10 variants, each carries base fields
//! (`timestamp`, optional `note`) plus variant-specific fields.
//!
//! `RejectReason` is the 5-value enum for rejected source attempts.
//!
//! The jsonl reader is **line-tolerant**: malformed lines and unknown event
//! values are skipped with stderr warnings (see `read_events`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    FetchFailed,
    WrongUrl,
    EmptyContent,
    ApiError,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SessionEvent {
    SessionCreated {
        timestamp: DateTime<Utc>,
        slug: String,
        topic: String,
        preset: String,
        session_dir_abs: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    SourceAttempted {
        timestamp: DateTime<Utc>,
        url: String,
        route_decision: RouteDecision,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    SourceAccepted {
        timestamp: DateTime<Utc>,
        url: String,
        kind: String,
        executor: String,
        raw_path: String,
        bytes: u64,
        trust_score: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    SourceRejected {
        timestamp: DateTime<Utc>,
        url: String,
        kind: String,
        executor: String,
        reason: RejectReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed_bytes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rejected_raw_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    SynthesizeStarted {
        timestamp: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    SynthesizeCompleted {
        timestamp: DateTime<Utc>,
        report_json_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report_html_path: Option<String>,
        accepted_sources: u32,
        rejected_sources: u32,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    SynthesizeFailed {
        timestamp: DateTime<Utc>,
        stage: SynthesizeStage,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    SessionClosed {
        timestamp: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    SessionRemoved {
        timestamp: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    SessionResumed {
        timestamp: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },

    // ── Autoresearch loop events ─────────────────────────────────────────
    // These are written only when `research loop` is invoked (feature:
    // autoresearch), but live in the canonical SessionEvent enum so the
    // event log stays closed — readers match all variants exhaustively.
    LoopStarted {
        timestamp: DateTime<Utc>,
        provider: String,
        iterations: u32,
        max_actions: u32,
        dry_run: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    LoopStep {
        timestamp: DateTime<Utc>,
        iteration: u32,
        reasoning: String,
        actions_requested: u32,
        actions_executed: u32,
        actions_rejected: u32,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    LoopCompleted {
        timestamp: DateTime<Utc>,
        reason: String,
        iterations_run: u32,
        actions_executed_total: u32,
        report_ready: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },

    /// v2: a previously-fetched source has been digested into a specific
    /// section of session.md. Subsequent prompt builds filter these URLs
    /// out of the "unread sources" block so Claude doesn't re-summarize
    /// the same paper every iteration.
    SourceDigested {
        timestamp: DateTime<Utc>,
        iteration: u32,
        url: String,
        into_section: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },

    /// v2: a `## Plan` block was authored by the agent (or overwritten).
    /// The body itself lives in `session.md` — this event records *that*
    /// and *when* a plan landed, plus its size for audit.
    PlanWritten {
        timestamp: DateTime<Utc>,
        iteration: u32,
        body_chars: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteDecision {
    pub executor: String,
    pub kind: String,
    pub command_template: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SynthesizeStage {
    Build,
    Render,
}

/// Read a session.jsonl file and return valid events. Malformed lines and
/// unknown event types are **skipped with a stderr warning**; I/O errors
/// against the file itself bubble up.
pub fn read_events(path: &Path) -> std::io::Result<Vec<SessionEvent>> {
    let f = std::fs::File::open(path)?;
    let mut events = Vec::new();
    let reader = BufReader::new(f);
    for (idx, line_res) in reader.lines().enumerate() {
        let line_no = idx + 1;
        let line = match line_res {
            Ok(l) => l,
            Err(e) => {
                eprintln!("⚠ session.jsonl line {line_no} read error: {e}, skipped");
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<SessionEvent>(&line) {
            Ok(ev) => events.push(ev),
            Err(e) => {
                eprintln!(
                    "⚠ session.jsonl line {line_no} malformed or unknown event: {e}, skipped"
                );
            }
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 19, 12, 0, 0).unwrap()
    }

    #[test]
    fn round_trip_all_variants() {
        let events = vec![
            SessionEvent::SessionCreated {
                timestamp: ts(),
                slug: "foo".into(),
                topic: "topic".into(),
                preset: "tech".into(),
                session_dir_abs: "/tmp/foo".into(),
                note: None,
            },
            SessionEvent::SourceAttempted {
                timestamp: ts(),
                url: "https://example.com".into(),
                route_decision: RouteDecision {
                    executor: "postagent".into(),
                    kind: "hn-item".into(),
                    command_template: "...".into(),
                },
                note: None,
            },
            SessionEvent::SourceAccepted {
                timestamp: ts(),
                url: "https://example.com".into(),
                kind: "hn-item".into(),
                executor: "postagent".into(),
                raw_path: "raw/1-hn-item.json".into(),
                bytes: 1234,
                trust_score: 2.0,
                note: None,
            },
            SessionEvent::SourceRejected {
                timestamp: ts(),
                url: "https://example.com".into(),
                kind: "browser-fallback".into(),
                executor: "browser".into(),
                reason: RejectReason::WrongUrl,
                observed_url: Some("about:blank".into()),
                observed_bytes: Some(0),
                rejected_raw_path: None,
                note: None,
            },
            SessionEvent::SynthesizeStarted { timestamp: ts(), note: None },
            SessionEvent::SynthesizeCompleted {
                timestamp: ts(),
                report_json_path: "report.json".into(),
                report_html_path: Some("report.html".into()),
                accepted_sources: 3,
                rejected_sources: 1,
                duration_ms: 500,
                note: None,
            },
            SessionEvent::SynthesizeFailed {
                timestamp: ts(),
                stage: SynthesizeStage::Render,
                reason: "json-ui not found".into(),
                note: None,
            },
            SessionEvent::SessionClosed { timestamp: ts(), note: None },
            SessionEvent::SessionRemoved { timestamp: ts(), note: None },
            SessionEvent::SessionResumed { timestamp: ts(), note: None },
        ];
        assert_eq!(events.len(), 10, "must have 10 variants");
        for ev in events {
            let s = serde_json::to_string(&ev).unwrap();
            let back: SessionEvent = serde_json::from_str(&s).unwrap();
            assert_eq!(back, ev);
        }
    }

    #[test]
    fn reject_reason_has_5_values() {
        let all = [
            RejectReason::FetchFailed,
            RejectReason::WrongUrl,
            RejectReason::EmptyContent,
            RejectReason::ApiError,
            RejectReason::Duplicate,
        ];
        // Round-trip each.
        for r in all {
            let s = serde_json::to_string(&r).unwrap();
            let back: RejectReason = serde_json::from_str(&s).unwrap();
            assert_eq!(back, r);
        }
    }

    #[test]
    fn read_events_is_line_tolerant() {
        use std::io::Write;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut f = tmp.reopen().unwrap();
        writeln!(f, r#"{{"event":"session_created","timestamp":"2026-04-19T12:00:00Z","slug":"foo","topic":"t","preset":"tech","session_dir_abs":"/tmp"}}"#).unwrap();
        writeln!(f, r#"{{"event":"source_accepted","timestamp":"2026-04-19T12:00:00Z""#).unwrap(); // truncated
        writeln!(f, r#"{{"event":"source_accepted","timestamp":"2026-04-19T12:00:00Z","url":"u","kind":"k","executor":"postagent","raw_path":"r","bytes":1,"trust_score":2.0}}"#).unwrap();
        writeln!(f, r#"{{"event":"unknown_future_event","timestamp":"2026-04-19T12:00:00Z"}}"#).unwrap();
        let events = read_events(tmp.path()).unwrap();
        assert_eq!(events.len(), 2, "only 2 valid events should come through");
    }
}
