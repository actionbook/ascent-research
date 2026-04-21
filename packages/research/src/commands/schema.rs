//! `research schema {show, edit}` — human-facing commands for the
//! per-session `SCHEMA.md`.
//!
//! `schema show` prints the file (or the starter template, hinting at
//! the path). `schema edit` launches `$EDITOR` against the file and,
//! if the file changed, emits a `SchemaUpdated` jsonl event so the
//! loop picks it up on the next iteration.

use std::fs;
use std::process::Command;

use chrono::Utc;
use serde_json::json;

use crate::output::Envelope;
use crate::session::{active, config, event::SessionEvent, layout, log, schema};

const CMD_SHOW: &str = "research schema show";
const CMD_EDIT: &str = "research schema edit";

/// `research schema show [slug]` — print the session's SCHEMA.md.
///
/// Returns the raw body plus metadata so the CLI-text renderer can
/// print a useful "schema not yet created" hint when absent without
/// making the command fail-loud in JSON mode.
pub fn run_show(slug_arg: Option<&str>) -> Envelope {
    let slug = match resolve_slug(slug_arg, CMD_SHOW) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let path = layout::session_schema_md(&slug);
    let exists = path.exists();
    let body = schema::read(&slug).unwrap_or_default();
    Envelope::ok(
        CMD_SHOW,
        json!({
            "slug": slug,
            "path": path.display().to_string(),
            "exists": exists,
            "bytes": body.len(),
            "body": body,
        }),
    )
    .with_context(json!({ "session": slug }))
}

/// `research schema edit [slug]` — open `$EDITOR` on the schema file.
///
/// If the file is absent it's seeded with the starter template FIRST
/// (so the user opens something useful, not an empty buffer). After
/// the editor exits we compare the post-edit mtime to pre-edit; if it
/// changed we log a `SchemaUpdated` event.
pub fn run_edit(slug_arg: Option<&str>) -> Envelope {
    let slug = match resolve_slug(slug_arg, CMD_EDIT) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let path = layout::session_schema_md(&slug);
    if let Err(e) = schema::write_starter_if_absent(&slug) {
        return Envelope::fail(CMD_EDIT, "IO_ERROR", format!("seed SCHEMA.md: {e}"));
    }
    let before = fs::metadata(&path).ok().and_then(|m| m.modified().ok());
    let before_body = fs::read_to_string(&path).unwrap_or_default();

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    // Pass the editor's own argv through a shell so users can keep
    // habits like `EDITOR='code -w'`. The argument is the absolute
    // schema path we control — not user input — so command injection
    // requires malicious `$EDITOR`, which is already trusted.
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"{}\"", path.display()))
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            return Envelope::fail(
                CMD_EDIT,
                "EDITOR_FAILED",
                format!("editor '{editor}' exited with status {s}"),
            );
        }
        Err(e) => {
            return Envelope::fail(
                CMD_EDIT,
                "EDITOR_NOT_FOUND",
                format!("could not launch '{editor}': {e}"),
            );
        }
    }

    let after_body = fs::read_to_string(&path).unwrap_or_default();
    let after = fs::metadata(&path).ok().and_then(|m| m.modified().ok());
    let changed = before != after || before_body != after_body;
    if changed {
        let ev = SessionEvent::SchemaUpdated {
            timestamp: Utc::now(),
            body_chars: after_body.chars().count() as u32,
            note: None,
        };
        if let Err(e) = log::append(&slug, &ev) {
            eprintln!("⚠ warning: could not append schema_updated event: {e}");
        }
    }

    Envelope::ok(
        CMD_EDIT,
        json!({
            "slug": slug,
            "path": path.display().to_string(),
            "changed": changed,
            "bytes": after_body.len(),
        }),
    )
    .with_context(json!({ "session": slug }))
}

fn resolve_slug(slug_arg: Option<&str>, cmd: &'static str) -> Result<String, Envelope> {
    let slug = match slug_arg {
        Some(s) => s.to_string(),
        None => match active::get_active() {
            Some(s) => s,
            None => {
                return Err(Envelope::fail(
                    cmd,
                    "NO_ACTIVE_SESSION",
                    "no active session — pass --slug or run `research new` first",
                ));
            }
        },
    };
    if !config::exists(&slug) {
        return Err(Envelope::fail(cmd, "SESSION_NOT_FOUND", format!("no session '{slug}'"))
            .with_context(json!({ "session": slug })));
    }
    Ok(slug)
}
