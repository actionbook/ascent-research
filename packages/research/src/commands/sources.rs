use serde_json::{Value, json};

use crate::output::Envelope;
use crate::session::{active, config, event::SessionEvent, log};

const CMD: &str = "research sources";

pub fn run(slug_arg: Option<&str>, show_rejected: bool) -> Envelope {
    let slug = match slug_arg {
        Some(s) => s.to_string(),
        None => match active::get_active() {
            Some(s) => s,
            None => {
                return Envelope::fail(
                    CMD,
                    "NO_ACTIVE_SESSION",
                    "no active session — pass <slug> or run `research new` first",
                );
            }
        },
    };

    if !config::exists(&slug) {
        return Envelope::fail(CMD, "SESSION_NOT_FOUND", format!("no session '{slug}'"))
            .with_context(json!({ "session": slug }));
    }

    let events = log::read_all(&slug).unwrap_or_default();
    let mut accepted: Vec<Value> = Vec::new();
    let mut rejected: Vec<Value> = Vec::new();
    for ev in &events {
        match ev {
            SessionEvent::SourceAccepted {
                timestamp,
                url,
                kind,
                executor,
                raw_path,
                bytes,
                trust_score,
                ..
            } => accepted.push(json!({
                "timestamp": timestamp,
                "url": url,
                "kind": kind,
                "executor": executor,
                "raw_path": raw_path,
                "bytes": bytes,
                "trust_score": trust_score,
            })),
            SessionEvent::SourceRejected {
                timestamp,
                url,
                kind,
                executor,
                reason,
                observed_url,
                observed_bytes,
                rejected_raw_path,
                ..
            } if show_rejected => rejected.push(json!({
                "timestamp": timestamp,
                "url": url,
                "kind": kind,
                "executor": executor,
                "reason": reason,
                "observed_url": observed_url,
                "observed_bytes": observed_bytes,
                "rejected_raw_path": rejected_raw_path,
            })),
            _ => {}
        }
    }

    let mut data = json!({ "accepted": accepted });
    if show_rejected {
        data["rejected"] = json!(rejected);
    }
    Envelope::ok(CMD, data).with_context(json!({ "session": slug }))
}
