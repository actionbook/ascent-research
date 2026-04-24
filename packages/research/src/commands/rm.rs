use serde_json::json;
use std::fs;
use std::io::IsTerminal;

use crate::output::Envelope;
use crate::session::{active, config, event::SessionEvent, layout, log};

const CMD: &str = "research rm";

pub fn run(slug: &str, force: bool) -> Envelope {
    if !layout::session_dir(slug).exists() {
        return Envelope::fail(CMD, "SESSION_NOT_FOUND", format!("no session '{slug}'"))
            .with_context(json!({ "session": slug }));
    }

    // If any source was accepted, require confirmation unless --force.
    let accepted_count = log::read_all(slug)
        .map(|evs| {
            evs.iter()
                .filter(|e| matches!(e, SessionEvent::SourceAccepted { .. }))
                .count() as u32
        })
        .unwrap_or(0);

    if accepted_count > 0 && !force {
        if !std::io::stdin().is_terminal() {
            return Envelope::fail(
                CMD,
                "CONFIRMATION_REQUIRED",
                format!(
                    "session '{slug}' has {accepted_count} accepted sources; pass --force to delete"
                ),
            )
            .with_context(json!({ "session": slug }))
            .with_details(json!({ "accepted_sources": accepted_count }));
        }
        // TTY: simple y/n prompt
        eprintln!("Session '{slug}' has {accepted_count} accepted sources. Delete? [y/N] ");
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err()
            || !matches!(line.trim(), "y" | "Y" | "yes" | "YES")
        {
            return Envelope::fail(CMD, "CONFIRMATION_DECLINED", "user declined");
        }
    }

    // If this slug was active, clear.
    if active::get_active().as_deref() == Some(slug) {
        let _ = active::clear_active();
    }

    // Append session_removed event FIRST so it's captured before we nuke
    // the file. (This won't survive rm but we want the shape consistent
    // if someone snapshots before the dir is gone.)
    if config::exists(slug) {
        let ev = SessionEvent::SessionRemoved {
            timestamp: chrono::Utc::now(),
            note: None,
        };
        let _ = log::append(slug, &ev);
    }

    if let Err(e) = fs::remove_dir_all(layout::session_dir(slug)) {
        return Envelope::fail(CMD, "IO_ERROR", format!("remove session dir: {e}"));
    }

    Envelope::ok(CMD, json!({ "slug": slug, "removed": true }))
        .with_context(json!({ "session": slug }))
}
