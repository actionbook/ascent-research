//! `research add-local <path>` — bulk local source ingest.
//!
//! Walks a file or directory tree, applies globset include/exclude
//! patterns, and attaches each accepted file as a source via the same
//! pipeline as `research add file:///...`. Per-file and per-walk size
//! caps keep a sprawling source tree (tokio/src, a monorepo) from
//! blowing out the session.
//!
//! Emits one `SourceAccepted` per accepted file + a single envelope
//! summarizing accepted / skipped counts and total bytes.

use serde_json::{Value, json};

use crate::fetch::local;
use crate::output::Envelope;
use crate::session::{active, config};

const CMD: &str = "research add-local";

/// Default per-file cap (mirrors `local::DEFAULT_MAX_FILE_BYTES`).
const DEFAULT_MAX_FILE_BYTES: u64 = local::DEFAULT_MAX_FILE_BYTES;
/// Default per-walk cap — 2 MiB keeps a typical crate src tree within
/// reason while leaving plenty of room for careful manual runs.
const DEFAULT_MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024;

pub fn run(
    path: &str,
    slug_arg: Option<&str>,
    globs: &[String],
    max_file_bytes: Option<u64>,
    max_total_bytes: Option<u64>,
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
            .with_context(json!({ "session": slug, "path": path }));
    }

    // Normalize the input path — accept `file://...`, abs, relative, ~/...
    let abs_path = match normalize_path(path) {
        Some(p) => p,
        None => {
            return Envelope::fail(
                CMD,
                "INVALID_ARGUMENT",
                format!("path '{path}' is not a recognized local address"),
            );
        }
    };
    if !abs_path.exists() {
        return Envelope::fail(
            CMD,
            "PATH_NOT_FOUND",
            format!("'{}' does not exist", abs_path.display()),
        )
        .with_context(json!({ "path": path }));
    }

    let file_cap = max_file_bytes.unwrap_or(DEFAULT_MAX_FILE_BYTES);
    let total_cap = max_total_bytes.unwrap_or(DEFAULT_MAX_TOTAL_BYTES);

    let walk = match local::walk_tree(&abs_path, globs, file_cap, total_cap) {
        Ok(w) => w,
        Err(e) => {
            return Envelope::fail(
                CMD,
                "WALK_FAILED",
                format!("walk '{}': {e}", abs_path.display()),
            );
        }
    };

    let mut accepted_results: Vec<Value> = Vec::new();
    let mut failure_results: Vec<Value> = Vec::new();

    // Reuse the existing `add` flow per file — same raw write, same
    // session.jsonl event, same smell test, same trust score. Cost is
    // one toml read per file; that's fine for a handful to a few
    // hundred files and keeps the code path single.
    for file in &walk.accepted {
        let url = format!("file://{}", file.path.display());
        let env = crate::commands::add::run(
            &url,
            Some(&slug),
            None,  // default timeout
            false, // readable — meaningless for local
            false,
            None, // min-bytes — use default
            None, // on-short-body — default reject
        );
        if env.ok {
            accepted_results.push(json!({
                "url": url,
                "bytes": file.size,
            }));
        } else {
            failure_results.push(json!({
                "url": url,
                "error": env.error.as_ref().map(|e| e.code.clone()),
                "message": env.error.as_ref().map(|e| e.message.clone()),
            }));
        }
    }

    let skipped: Vec<Value> = walk
        .skipped
        .iter()
        .map(|s| {
            json!({
                "path": s.path.display().to_string(),
                "reason": s.reason,
            })
        })
        .collect();

    Envelope::ok(
        CMD,
        json!({
            "root": abs_path.display().to_string(),
            "accepted": accepted_results,
            "accepted_count": accepted_results.len(),
            "failed": failure_results,
            "failed_count": failure_results.len(),
            "skipped": skipped,
            "skipped_count": skipped.len(),
            "total_bytes_accepted": walk.total_bytes,
            "caps": {
                "max_file_bytes": file_cap,
                "max_total_bytes": total_cap,
            }
        }),
    )
    .with_context(json!({ "session": slug }))
}

fn normalize_path(input: &str) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let raw = if let Some(rest) = input.strip_prefix("file://") {
        match rest.find('/') {
            Some(0) => rest.to_string(),
            Some(i) => rest[i..].to_string(),
            None => return None,
        }
    } else if let Some(rest) = input.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()?;
        format!("{home}/{rest}")
    } else if input.starts_with('/') || input.starts_with("./") || input.starts_with("../") {
        input.to_string()
    } else {
        // Bare relative (no `./` prefix) — resolve against cwd so the
        // user can `research add-local tokio/src` from the parent.
        input.to_string()
    };
    let pb = PathBuf::from(raw);
    // Canonicalize when possible so jsonl + sources list are stable.
    Some(std::fs::canonicalize(&pb).unwrap_or(pb))
}
