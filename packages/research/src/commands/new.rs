use chrono::Utc;
use serde_json::json;
use std::collections::HashSet;
use std::fs;

use crate::output::Envelope;
use crate::session::{
    active, config,
    event::SessionEvent,
    layout, log, md_parser, md_template, schema,
    slug as slugmod,
};

const CMD: &str = "research new";
const PARENT_CHAIN_MAX: usize = 10;

pub fn run(
    topic: &str,
    preset: Option<&str>,
    slug_override: Option<&str>,
    force: bool,
    from: Option<&str>,
    extra_tags: &[String],
) -> Envelope {
    let preset = preset.unwrap_or("tech").to_string();
    let root = layout::research_root();
    if let Err(e) = fs::create_dir_all(&root) {
        return Envelope::fail(CMD, "IO_ERROR", format!("create research root: {e}"));
    }

    // Validate parent if --from given, resolve inherited tags, pull Overview.
    let (parent_overview, parent_tags) = match from {
        Some(parent_slug) => match load_parent(parent_slug) {
            Ok(x) => x,
            Err(env) => return env,
        },
        None => (None, Vec::new()),
    };

    let resolved = match slugmod::resolve_slug(topic, slug_override, &root) {
        Ok(s) => s,
        Err(slugmod::SlugError::Exists) if force && slug_override.is_some() => {
            let s = slug_override.unwrap().to_string();
            if let Err(e) = fs::remove_dir_all(root.join(&s)) {
                return Envelope::fail(CMD, "IO_ERROR", format!("remove existing {s}: {e}"));
            }
            s
        }
        Err(slugmod::SlugError::Exists) => {
            return Envelope::fail(
                CMD,
                "SLUG_EXISTS",
                format!(
                    "slug '{}' already exists — pass --force to overwrite or omit --slug to auto-derive",
                    slug_override.unwrap_or("")
                ),
            )
            .with_context(json!({ "slug": slug_override }));
        }
        Err(slugmod::SlugError::Invalid(msg)) => {
            return Envelope::fail(CMD, "INVALID_ARGUMENT", msg);
        }
    };

    // Cycle check: walking parent chain should not hit `resolved`.
    if let Some(parent) = from {
        if detect_cycle(parent, &resolved) {
            return Envelope::fail(
                CMD,
                "CYCLE_DETECTED",
                format!(
                    "parent chain starting at '{parent}' would create a cycle back to '{resolved}'"
                ),
            );
        }
    }

    let dir = layout::session_dir(&resolved);
    if let Err(e) = fs::create_dir_all(layout::session_raw_dir(&resolved)) {
        return Envelope::fail(CMD, "IO_ERROR", format!("create session dir: {e}"));
    }

    // Merge tags: inherited ∪ explicit (preserve ordering; dedupe).
    let mut tags_merged: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for t in parent_tags.iter().chain(extra_tags.iter()) {
        if seen.insert(t.clone()) {
            tags_merged.push(t.clone());
        }
    }

    let mut cfg = config::SessionConfig::new(resolved.clone(), topic, preset.clone());
    cfg.parent_slug = from.map(String::from);
    cfg.tags = tags_merged.clone();

    if let Err(e) = config::write(&resolved, &cfg) {
        let _ = fs::remove_dir_all(&dir);
        return Envelope::fail(CMD, "IO_ERROR", format!("write session.toml: {e}"));
    }

    let md = md_template::render_with_context(
        topic,
        &preset,
        from,
        parent_overview.as_deref(),
    );
    if let Err(e) = fs::write(layout::session_md(&resolved), md) {
        let _ = fs::remove_dir_all(&dir);
        return Envelope::fail(CMD, "IO_ERROR", format!("write session.md: {e}"));
    }

    // Seed SCHEMA.md with the starter template so every session has
    // user-editable loop guidance from iteration zero. Non-fatal — if
    // we can't write it we just log and move on; the loop treats a
    // missing schema as "use defaults."
    if let Err(e) = schema::write_starter_if_absent(&resolved) {
        eprintln!("⚠ warning: could not seed SCHEMA.md: {e}");
    }

    let ev = SessionEvent::SessionCreated {
        timestamp: Utc::now(),
        slug: resolved.clone(),
        topic: topic.to_string(),
        preset: preset.clone(),
        session_dir_abs: dir
            .canonicalize()
            .unwrap_or(dir.clone())
            .to_string_lossy()
            .into_owned(),
        note: None,
    };
    if let Err(e) = log::append(&resolved, &ev) {
        let _ = fs::remove_dir_all(&dir);
        return Envelope::fail(CMD, "IO_ERROR", format!("append session_created: {e}"));
    }

    if let Err(e) = active::set_active(&resolved) {
        return Envelope::fail(CMD, "IO_ERROR", format!("set active: {e}"));
    }

    Envelope::ok(
        CMD,
        json!({
            "slug": resolved,
            "session_dir": dir.to_string_lossy(),
            "topic": topic,
            "preset": preset,
            "parent_slug": from,
            "tags": tags_merged,
            "active": true,
        }),
    )
    .with_context(json!({ "session": resolved }))
}

/// Load parent's Overview + tags for inheritance.
fn load_parent(parent_slug: &str) -> Result<(Option<String>, Vec<String>), Envelope> {
    if !config::exists(parent_slug) {
        return Err(Envelope::fail(
            CMD,
            "PARENT_NOT_FOUND",
            format!("parent session '{parent_slug}' does not exist"),
        )
        .with_context(json!({ "parent_slug": parent_slug })));
    }
    let parent_cfg = match config::read(parent_slug) {
        Ok(c) => c,
        Err(e) => {
            return Err(Envelope::fail(
                CMD,
                "IO_ERROR",
                format!("read parent session.toml: {e}"),
            ));
        }
    };
    let md = fs::read_to_string(layout::session_md(parent_slug)).unwrap_or_default();
    let overview = md_parser::extract_overview(&md);
    Ok((overview, parent_cfg.tags))
}

/// Walk the parent chain from `start` and check whether it eventually reaches
/// `target_slug`. Bounded to PARENT_CHAIN_MAX hops.
fn detect_cycle(start: &str, target_slug: &str) -> bool {
    let mut cur = start.to_string();
    let mut seen = HashSet::new();
    for _ in 0..PARENT_CHAIN_MAX {
        if cur == target_slug {
            return true;
        }
        if !seen.insert(cur.clone()) {
            // pathological existing cycle between ancestors — bail safely
            return true;
        }
        if !config::exists(&cur) {
            return false;
        }
        match config::read(&cur) {
            Ok(c) => match c.parent_slug {
                Some(p) => cur = p,
                None => return false,
            },
            Err(_) => return false,
        }
    }
    true
}
