//! `research report <slug> --format <fmt>` — editorial report rendering.
//!
//! Phase A (this file, v1): scaffolding. Accepts `--format rich-html`, runs
//! active-slug + session-exists checks, loads session.md, substitutes the
//! embedded template with **stub** BODY/ASIDE/SOURCES content, writes
//! `report-rich.html` to the session directory.
//!
//! Phase B adds real markdown→HTML with aside/diagram/section-num conventions.
//! Phase C replaces stub sources with a real list built from session.jsonl.

use chrono::Utc;
use serde_json::json;
use std::fs;
use std::io::IsTerminal;
use std::process::Command;
use std::time::Instant;

use crate::commands::coverage;
use crate::output::Envelope;
use crate::report::brief_md::{self, BriefInput};
use crate::report::markdown::{self, RenderError};
use crate::report::sources;
use crate::report::template::{self, Slots};
use crate::session::{active, config, layout};

const CMD: &str = "research report";

/// Supported format values. Keep in one place so unknown-format errors can
/// list what the current binary can actually produce.
const SUPPORTED_FORMATS: &[&str] = &["rich-html", "brief-md"];

/// Formats named in the spec that will be wired up later. Kept separate from
/// `SUPPORTED_FORMATS` so the envelope error can say "recognized but not yet
/// implemented" vs "never heard of it".
const FUTURE_FORMATS: &[&str] = &["slides-reveal", "json-export"];

pub fn run(
    slug_arg: Option<&str>,
    format: &str,
    open: bool,
    _no_open: bool,
    stdout: bool,
    output: Option<&str>,
) -> Envelope {
    // ── Format validation ─────────────────────────────────────────────────
    if !SUPPORTED_FORMATS.contains(&format) {
        return if FUTURE_FORMATS.contains(&format) {
            Envelope::fail(
                CMD,
                "FORMAT_NOT_IMPLEMENTED",
                format!("format '{format}' is declared in the spec but not yet implemented"),
            )
            .with_details(json!({ "requested": format, "supported": SUPPORTED_FORMATS }))
        } else {
            Envelope::fail(
                CMD,
                "FORMAT_UNSUPPORTED",
                format!("unknown format '{format}'"),
            )
            .with_details(json!({ "requested": format, "supported": SUPPORTED_FORMATS }))
        };
    }

    // ── Slug resolution ───────────────────────────────────────────────────
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

    let cfg = match config::read(&slug) {
        Ok(c) => c,
        Err(e) => return Envelope::fail(CMD, "IO_ERROR", format!("read session.toml: {e}")),
    };

    // Verify session.md exists (content parsing happens in Phase B).
    let md = match fs::read_to_string(layout::session_md(&slug)) {
        Ok(s) => s,
        Err(e) => return Envelope::fail(CMD, "IO_ERROR", format!("read session.md: {e}")),
    };

    // Phase A MISSING_OVERVIEW preflight — the md must have a non-placeholder
    // Overview. Phase B will do proper parsing; this is a minimal gate.
    if !has_non_empty_overview(&md) {
        return Envelope::fail(
            CMD,
            "MISSING_OVERVIEW",
            "session.md lacks a non-placeholder `## Overview` section — edit it and retry",
        )
        .with_context(json!({ "session": slug }));
    }

    let start = Instant::now();

    // ── Format: brief-md ──────────────────────────────────────────────────
    if format == "brief-md" {
        return run_brief_md(&slug, &cfg.topic, &md, start, stdout, output);
    }

    // ── Assemble slots (Phase A: stubbed body/aside/sources) ─────────────
    let tags_str = if cfg.tags.is_empty() {
        String::new()
    } else {
        format!(" · tagged {}", cfg.tags.join(", "))
    };
    let subtitle = format!(
        "Session: <code>{}</code>{} · preset <code>{}</code>",
        slug, tags_str, cfg.preset
    );

    // Phase B: markdown → HTML with aside / diagram / section-num conventions.
    let session_dir = layout::session_dir(&slug);
    let rendered = match markdown::render_body(&md, &session_dir) {
        Ok(r) => r,
        Err(RenderError::DiagramOutOfBounds(p)) => {
            return Envelope::fail(
                CMD,
                "DIAGRAM_OUT_OF_BOUNDS",
                format!(
                    "diagram path '{}' resolves outside session_dir/diagrams/",
                    p.display()
                ),
            )
            .with_context(json!({ "session": slug }));
        }
    };

    if let Some(env) = report_ready_preflight(&slug) {
        return env;
    }

    // Phase C: build Sources section from session.jsonl (authoritative fact
    // stream), not from the session.md sources block (human-readable cache).
    let sources_section = sources::build_from_jsonl(&layout::session_jsonl(&slug));
    let mut warnings = rendered.warnings.clone();
    warnings.extend(sources_section.warnings.iter().cloned());
    let diagrams_inlined = rendered.diagrams_inlined;
    let sources_count = sources_section.count;
    let total_bytes = sources_section.total_bytes;

    let session_footer = format!(
        "Session · {} · {} accepted source{} · {} bytes",
        session_dir.display(),
        sources_count,
        if sources_count == 1 { "" } else { "s" },
        total_bytes,
    );

    let slots = Slots {
        title: cfg.topic.clone(),
        subtitle,
        aside_quote: rendered.aside_html,
        body_html: rendered.body_html,
        sources_html: sources_section.html,
        generated_at: Utc::now().to_rfc3339(),
        session_footer,
    };

    let html = template::render(&slots);

    // ── Write output ──────────────────────────────────────────────────────
    let output_path = layout::session_dir(&slug).join("report-rich.html");
    if let Err(e) = fs::write(&output_path, &html) {
        return Envelope::fail(CMD, "RENDER_FAILED", format!("write report: {e}"))
            .with_context(json!({ "session": slug }));
    }

    let duration_ms = start.elapsed().as_millis() as u64;

    // ── Optional open ─────────────────────────────────────────────────────
    let mut open_skipped: Option<&'static str> = None;
    if open {
        if should_skip_open() {
            open_skipped = Some("non-interactive environment");
            eprintln!("skipping open (non-interactive)");
        } else {
            let spawn_result = if cfg!(target_os = "macos") {
                Command::new("open").arg(&output_path).spawn()
            } else {
                Command::new("xdg-open").arg(&output_path).spawn()
            };
            if let Err(e) = spawn_result {
                eprintln!("⚠ open failed: {e}");
            }
        }
    }

    Envelope::ok(
        CMD,
        json!({
            "format": format,
            "output_path": output_path.display().to_string(),
            "bytes": html.len(),
            "duration_ms": duration_ms,
            "open_skipped": open_skipped,
            "warnings": warnings,
            "diagrams_inlined": diagrams_inlined,
            "sources_count": sources_count,
            "total_bytes": total_bytes,
            "phase": "C",
        }),
    )
    .with_context(json!({ "session": slug }))
}

fn report_ready_preflight(slug: &str) -> Option<Envelope> {
    let coverage = coverage::run(Some(slug));
    if !coverage.ok {
        let (reason, details) = if let Some(err) = coverage.error {
            (
                format!("coverage preflight failed: {}", err.message),
                err.details,
            )
        } else {
            (
                "coverage preflight failed".to_string(),
                serde_json::Value::Null,
            )
        };
        let mut env =
            Envelope::fail(CMD, "IO_ERROR", reason).with_context(json!({ "session": slug }));
        if !details.is_null() {
            env = env.with_details(details);
        }
        return Some(env);
    }

    if coverage.data["report_ready"] == json!(true) {
        return None;
    }

    Some(
        Envelope::fail(
            CMD,
            "REPORT_NOT_READY",
            "session does not satisfy `research coverage` gates — fix blockers and retry",
        )
        .with_context(json!({ "session": slug }))
        .with_details(json!({
            "report_ready": coverage.data["report_ready"].clone(),
            "report_ready_blockers": coverage.data["report_ready_blockers"].clone(),
        })),
    )
}

/// Render `--format brief-md`. Shares Overview gate + slug resolution with
/// the rich-html path but has its own output routing (stdout / explicit
/// --output / default `<session>/report-brief.md`).
fn run_brief_md(
    slug: &str,
    topic: &str,
    md: &str,
    start: Instant,
    stdout: bool,
    output: Option<&str>,
) -> Envelope {
    let jsonl_path = layout::session_jsonl(slug);
    let brief = brief_md::build(BriefInput {
        topic,
        slug,
        md,
        jsonl_path: &jsonl_path,
    });
    let bytes = brief.text.len() as u64;
    let duration_ms = start.elapsed().as_millis() as u64;

    let output_path: Option<std::path::PathBuf> = if stdout {
        print!("{}", brief.text);
        None
    } else {
        let path = match output {
            Some(p) => std::path::PathBuf::from(p),
            None => layout::session_dir(slug).join("report-brief.md"),
        };
        if let Err(e) = fs::write(&path, &brief.text) {
            return Envelope::fail(CMD, "RENDER_FAILED", format!("write brief: {e}"))
                .with_context(json!({ "session": slug }));
        }
        Some(path)
    };

    Envelope::ok(
        CMD,
        json!({
            "format": "brief-md",
            "output_path": output_path.as_ref().map(|p| p.display().to_string()),
            "stdout": stdout,
            "bytes": bytes,
            "warnings": brief.warnings,
            "duration_ms": duration_ms,
        }),
    )
    .with_context(json!({ "session": slug }))
}

/// Minimal Overview-presence check. Phase B replaces this with proper md_parser
/// integration that tolerates trailing newlines and ignores HTML comments.
fn has_non_empty_overview(md: &str) -> bool {
    let mut in_overview = false;
    for line in md.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## Overview") {
            in_overview = true;
            continue;
        }
        if in_overview {
            if trimmed.starts_with("## ") {
                // reached the next section with no real content
                return false;
            }
            if trimmed.is_empty() {
                continue;
            }
            // HTML comments (placeholders like `<!-- fill in… -->`) don't count
            if trimmed.starts_with("<!--") {
                continue;
            }
            return true;
        }
    }
    false
}

fn should_skip_open() -> bool {
    if std::env::var("RESEARCH_NO_OPEN").is_ok() {
        return true;
    }
    if std::env::var("SYNTHESIZE_NO_OPEN").is_ok() {
        return true;
    }
    if std::env::var("CI").is_ok() {
        return true;
    }
    !std::io::stdin().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_non_empty_overview_detects_placeholder() {
        let md = "# T\n\n## Overview\n<!-- fill in -->\n\n## Findings\n";
        assert!(!has_non_empty_overview(md));
    }

    #[test]
    fn has_non_empty_overview_detects_real_content() {
        let md = "# T\n\n## Overview\nReal paragraph here.\n\n## Findings\n";
        assert!(has_non_empty_overview(md));
    }

    #[test]
    fn has_non_empty_overview_when_overview_is_last_section() {
        let md = "# T\n\n## Overview\nReal content at the end of file.\n";
        assert!(has_non_empty_overview(md));
    }

    #[test]
    fn has_non_empty_overview_missing_section() {
        let md = "# T\n\n## Findings\nstuff\n";
        assert!(!has_non_empty_overview(md));
    }
}
