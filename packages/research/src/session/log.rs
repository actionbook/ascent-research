//! Append SessionEvent lines to session.jsonl with an advisory flock.
//!
//! Per foundation spec, all writes to session.jsonl must hold an
//! exclusive lock on `session.jsonl.lock` (not on session.jsonl itself,
//! so readers don't contend). Writes are line-delimited JSON + `\n`.
//!
//! `raw/<n>` numbering: callers read the current count under the same lock
//! to allocate the next `<n>` — see `allocate_next_raw_n`.

use std::fs::OpenOptions;
use std::io::Write;

use super::active::LockGuard;
use super::event::{self, SessionEvent};
use super::layout;

/// Append one event to the session's jsonl. Holds
/// `session.jsonl.lock` exclusively for the duration.
pub fn append(slug: &str, ev: &SessionEvent) -> std::io::Result<()> {
    let _guard = LockGuard::exclusive(layout::session_jsonl_lock(slug))?;
    let path = layout::session_jsonl(slug);
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    let line = serde_json::to_string(ev)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_data()?;
    Ok(())
}

/// Read all events (line-tolerant — see `event::read_events`).
pub fn read_all(slug: &str) -> std::io::Result<Vec<SessionEvent>> {
    event::read_events(&layout::session_jsonl(slug))
}

/// Allocate the next `<n>` for raw/ files. Counts `source_attempted`
/// events in the current jsonl + 1. Caller must hold the session.jsonl
/// lock during the read→write critical section that creates the matching
/// raw/ file; this helper only does the read side.
pub fn next_raw_index(events: &[SessionEvent]) -> u32 {
    events
        .iter()
        .filter(|e| matches!(e, SessionEvent::SourceAttempted { .. }))
        .count() as u32
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn next_raw_index_counts_attempts() {
        use super::super::event::RouteDecision;
        let attempt = SessionEvent::SourceAttempted {
            timestamp: Utc::now(),
            url: "https://example.com".into(),
            route_decision: RouteDecision {
                executor: "postagent".into(),
                kind: "hn-item".into(),
                command_template: "...".into(),
            },
            note: None,
        };
        assert_eq!(next_raw_index(&[]), 1);
        assert_eq!(next_raw_index(std::slice::from_ref(&attempt)), 2);
        assert_eq!(next_raw_index(&[attempt.clone(), attempt.clone()]), 3);
    }
}
