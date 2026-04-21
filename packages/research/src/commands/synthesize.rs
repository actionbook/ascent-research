use chrono::Utc;
use serde_json::json;
use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use crate::output::Envelope;
use crate::report::builder::{self, BuildError, ReportInput};
use crate::report::markdown::{self, RenderError};
use crate::report::sources;
use crate::report::template::{self, Slots};
use crate::session::{
    active, config,
    event::{SessionEvent, SynthesizeStage},
    layout, log,
};

const CMD: &str = "research synthesize";

pub fn run(slug_arg: Option<&str>, no_render: bool, open: bool, bilingual: bool) -> Envelope {
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

    let md = match fs::read_to_string(layout::session_md(&slug)) {
        Ok(s) => s,
        Err(e) => return Envelope::fail(CMD, "IO_ERROR", format!("read session.md: {e}")),
    };

    let events = log::read_all(&slug).unwrap_or_default();

    let start = Instant::now();
    let _ = log::append(
        &slug,
        &SessionEvent::SynthesizeStarted {
            timestamp: Utc::now(),
            note: None,
        },
    );

    let input = ReportInput {
        topic: &cfg.topic,
        preset: &cfg.preset,
        md: &md,
        events: &events,
    };
    let built = match builder::build(&input) {
        Ok(b) => b,
        Err(BuildError::MissingOverview) => {
            let _ = log::append(
                &slug,
                &SessionEvent::SynthesizeFailed {
                    timestamp: Utc::now(),
                    stage: SynthesizeStage::Build,
                    reason: "missing `## Overview` section".into(),
                    note: None,
                },
            );
            return Envelope::fail(
                CMD,
                "MISSING_OVERVIEW",
                "session.md lacks a non-placeholder `## Overview` section — edit it and retry",
            )
            .with_context(json!({ "session": slug }));
        }
    };

    let report_json_path = layout::session_report_json(&slug);
    let serialized = match serde_json::to_string_pretty(&built.json) {
        Ok(s) => s,
        Err(e) => {
            let _ = log::append(
                &slug,
                &SessionEvent::SynthesizeFailed {
                    timestamp: Utc::now(),
                    stage: SynthesizeStage::Build,
                    reason: format!("serialize: {e}"),
                    note: None,
                },
            );
            return Envelope::fail(CMD, "IO_ERROR", format!("serialize report: {e}"));
        }
    };
    if let Err(e) = fs::write(&report_json_path, &serialized) {
        let _ = log::append(
            &slug,
            &SessionEvent::SynthesizeFailed {
                timestamp: Utc::now(),
                stage: SynthesizeStage::Build,
                reason: format!("write: {e}"),
                note: None,
            },
        );
        return Envelope::fail(CMD, "IO_ERROR", format!("write report.json: {e}"));
    }

    // Render stage — rich-html only. The json-ui rendering path was
    // removed; `synthesize` now emits the same warm-paper editorial HTML
    // that `research report --format rich-html` produces, so there is a
    // single canonical report template across the project.
    let mut report_html_path: Option<String> = None;
    let mut render_error: Option<String> = None;
    let mut render_warnings: Vec<String> = Vec::new();
    if !no_render {
        match render_rich_html(&slug, &md, &cfg.topic, &cfg.tags, &cfg.preset, bilingual) {
            Ok((html_path, warnings)) => {
                report_html_path = Some(rel_path(&html_path));
                render_warnings = warnings;
            }
            Err(e) => render_error = Some(e),
        }
    }

    let duration_ms = start.elapsed().as_millis() as u64;

    if let Some(err) = &render_error {
        let _ = log::append(
            &slug,
            &SessionEvent::SynthesizeFailed {
                timestamp: Utc::now(),
                stage: SynthesizeStage::Render,
                reason: err.clone(),
                note: None,
            },
        );
    } else {
        let _ = log::append(
            &slug,
            &SessionEvent::SynthesizeCompleted {
                timestamp: Utc::now(),
                report_json_path: rel_path(&report_json_path),
                report_html_path: report_html_path.clone(),
                accepted_sources: built.accepted_count,
                rejected_sources: built.rejected_count,
                duration_ms,
                note: None,
            },
        );
    }

    // Maybe open.
    let mut open_skipped: Option<&'static str> = None;
    if open {
        if should_skip_open() {
            open_skipped = Some("non-interactive environment");
            eprintln!("skipping open (non-interactive)");
        } else if let Some(html) = &report_html_path {
            let html_abs = layout::session_dir(&slug).join(html.trim_start_matches(&format!("{slug}/")));
            let spawn_result = if cfg!(target_os = "macos") {
                Command::new("open").arg(&html_abs).spawn()
            } else {
                Command::new("xdg-open").arg(&html_abs).spawn()
            };
            if let Err(e) = spawn_result {
                eprintln!("⚠ open failed: {e}");
            }
        }
    }

    if let Some(err) = render_error {
        return Envelope::fail(CMD, "RENDER_FAILED", err)
            .with_context(json!({ "session": slug }))
            .with_details(json!({
                "report_json_path": rel_path(&report_json_path),
                "accepted_sources": built.accepted_count,
                "rejected_sources": built.rejected_count,
            }));
    }

    let mut all_warnings = built.warnings.clone();
    all_warnings.extend(render_warnings);

    Envelope::ok(
        CMD,
        json!({
            "report_json_path": rel_path(&report_json_path),
            "report_html_path": report_html_path,
            "accepted_sources": built.accepted_count,
            "rejected_sources": built.rejected_count,
            "duration_ms": duration_ms,
            "open_skipped": open_skipped,
            "warnings": all_warnings,
        }),
    )
    .with_context(json!({ "session": slug }))
}

/// Render the rich-html report to `<session>/report.html` using the same
/// pipeline as `research report --format rich-html`. Returns the path and
/// any non-fatal warnings (e.g. multiple-aside detection from markdown
/// render). `DiagramOutOfBounds` bubbles up as a fatal render error.
fn render_rich_html(
    slug: &str,
    md: &str,
    topic: &str,
    tags: &[String],
    preset: &str,
    bilingual: bool,
) -> Result<(PathBuf, Vec<String>), String> {
    let session_dir = layout::session_dir(slug);
    let rendered = markdown::render_body(md, &session_dir).map_err(|e| match e {
        RenderError::DiagramOutOfBounds(p) => format!(
            "diagram_out_of_bounds: '{}' resolves outside session_dir/diagrams/",
            p.display()
        ),
    })?;
    let sources_section = sources::build_from_jsonl(&layout::session_jsonl(slug));
    let mut warnings = rendered.warnings.clone();
    warnings.extend(sources_section.warnings.iter().cloned());

    let body_html = if bilingual {
        match crate::report::bilingual::inject_zh_translations(&rendered.body_html) {
            Ok((augmented, note)) => {
                if let Some(n) = note {
                    warnings.push(n);
                }
                augmented
            }
            Err(e) => {
                warnings.push(format!("bilingual_skipped: {e}"));
                rendered.body_html.clone()
            }
        }
    } else {
        rendered.body_html.clone()
    };

    let tags_str = if tags.is_empty() {
        String::new()
    } else {
        format!(" · tagged {}", tags.join(", "))
    };
    let subtitle = format!(
        "Session: <code>{slug}</code>{tags_str} · preset <code>{preset}</code>"
    );
    let session_footer = format!(
        "Session · {} · {} accepted source{} · {} bytes",
        session_dir.display(),
        sources_section.count,
        if sources_section.count == 1 { "" } else { "s" },
        sources_section.total_bytes,
    );

    let slots = Slots {
        title: topic.to_string(),
        subtitle,
        aside_quote: rendered.aside_html,
        body_html,
        sources_html: sources_section.html,
        generated_at: Utc::now().to_rfc3339(),
        session_footer,
    };
    let html = template::render(&slots);

    let html_path = layout::session_dir(slug).join("report.html");
    fs::write(&html_path, &html).map_err(|e| format!("write report.html: {e}"))?;
    Ok((html_path, warnings))
}

fn should_skip_open() -> bool {
    if std::env::var("SYNTHESIZE_NO_OPEN").is_ok() {
        return true;
    }
    if std::env::var("CI").is_ok() {
        return true;
    }
    !std::io::stdin().is_terminal()
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
