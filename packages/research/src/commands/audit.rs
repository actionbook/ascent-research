use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::output::Envelope;
use crate::session::{
    active, config,
    event::{FactCheckOutcome, SessionEvent, ToolCallStatus},
    layout, log,
};

const CMD: &str = "research audit";

#[derive(Debug, Default)]
struct ToolTrace {
    call_id: String,
    started: bool,
    hand: Option<String>,
    tool: Option<String>,
    input_summary: Option<String>,
    status: Option<ToolCallStatus>,
    duration_ms: Option<u64>,
    output_summary: Option<String>,
    artifact_refs: Vec<String>,
    error_code: Option<String>,
}

pub fn run(slug_arg: Option<&str>) -> Envelope {
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
    let events = match log::read_all(&slug) {
        Ok(events) => events,
        Err(e) => return Envelope::fail(CMD, "IO_ERROR", format!("read session.jsonl: {e}")),
    };

    let fact_check_required = cfg.tags.iter().any(|tag| tag == "fact-check");
    let mut accepted_sources = HashSet::new();
    let mut digested_sources = HashSet::new();
    let mut sources_attempted = 0usize;
    let mut sources_accepted = 0usize;
    let mut sources_rejected = 0usize;

    let mut tools_started = 0usize;
    let mut tools_completed = 0usize;
    let mut tools_ok = 0usize;
    let mut tools_error = 0usize;
    let mut tool_order = Vec::new();
    let mut tool_calls: BTreeMap<String, ToolTrace> = BTreeMap::new();

    let mut fact_checks = Vec::new();
    let mut fact_supported = 0usize;
    let mut fact_refuted = 0usize;
    let mut fact_uncertain = 0usize;

    let mut synth_started = 0usize;
    let mut synth_completed = 0usize;
    let mut synth_failed = 0usize;
    let mut synth_bilingual_started = 0usize;
    let mut latest_bilingual_provider: Option<String> = None;
    let mut latest_report_json: Option<String> = None;
    let mut latest_report_html: Option<String> = None;
    let mut latest_synth_failure: Option<String> = None;

    let mut loop_started = 0usize;
    let mut loop_steps = 0usize;
    let mut loop_completed = 0usize;
    let mut last_loop_reason: Option<String> = None;
    let mut last_loop_report_ready: Option<bool> = None;

    for ev in &events {
        match ev {
            SessionEvent::SourceAttempted { .. } => sources_attempted += 1,
            SessionEvent::SourceAccepted { url, .. } => {
                sources_accepted += 1;
                accepted_sources.insert(url.clone());
            }
            SessionEvent::SourceDigested { url, .. } => {
                digested_sources.insert(url.clone());
            }
            SessionEvent::SourceRejected { .. } => sources_rejected += 1,
            SessionEvent::ToolCallStarted {
                call_id,
                hand,
                tool,
                input_summary,
                ..
            } => {
                tools_started += 1;
                if !tool_calls.contains_key(call_id) {
                    tool_order.push(call_id.clone());
                }
                let trace = tool_calls.entry(call_id.clone()).or_default();
                trace.call_id = call_id.clone();
                trace.started = true;
                trace.hand = Some(hand.clone());
                trace.tool = Some(tool.clone());
                trace.input_summary = Some(input_summary.clone());
            }
            SessionEvent::ToolCallCompleted {
                call_id,
                status,
                duration_ms,
                output_summary,
                artifact_refs,
                error_code,
                ..
            } => {
                tools_completed += 1;
                match status {
                    ToolCallStatus::Ok => tools_ok += 1,
                    ToolCallStatus::Error => tools_error += 1,
                }
                if !tool_calls.contains_key(call_id) {
                    tool_order.push(call_id.clone());
                }
                let trace = tool_calls.entry(call_id.clone()).or_default();
                trace.call_id = call_id.clone();
                trace.status = Some(*status);
                trace.duration_ms = Some(*duration_ms);
                trace.output_summary = Some(output_summary.clone());
                trace.artifact_refs = artifact_refs.clone();
                trace.error_code = error_code.clone();
            }
            SessionEvent::FactChecked {
                claim,
                query,
                sources,
                outcome,
                into_section,
                note,
                ..
            } => {
                match outcome {
                    FactCheckOutcome::Supported => fact_supported += 1,
                    FactCheckOutcome::Refuted => fact_refuted += 1,
                    FactCheckOutcome::Uncertain => fact_uncertain += 1,
                }
                fact_checks.push(json!({
                    "claim": claim,
                    "query": query,
                    "sources": sources,
                    "outcome": fact_outcome(*outcome),
                    "into_section": into_section,
                    "note": note,
                }));
            }
            SessionEvent::SynthesizeStarted {
                bilingual,
                bilingual_provider,
                ..
            } => {
                synth_started += 1;
                if *bilingual {
                    synth_bilingual_started += 1;
                    latest_bilingual_provider = bilingual_provider.clone();
                }
            }
            SessionEvent::SynthesizeCompleted {
                report_json_path,
                report_html_path,
                ..
            } => {
                synth_completed += 1;
                latest_report_json = Some(report_json_path.clone());
                latest_report_html = report_html_path.clone();
            }
            SessionEvent::SynthesizeFailed { reason, .. } => {
                synth_failed += 1;
                latest_synth_failure = Some(reason.clone());
            }
            SessionEvent::LoopStarted { .. } => loop_started += 1,
            SessionEvent::LoopStep { .. } => loop_steps += 1,
            SessionEvent::LoopCompleted {
                reason,
                report_ready,
                ..
            } => {
                loop_completed += 1;
                last_loop_reason = Some(reason.clone());
                last_loop_report_ready = Some(*report_ready);
            }
            _ => {}
        }
    }

    let tool_call_items: Vec<Value> = tool_order
        .iter()
        .filter_map(|call_id| tool_calls.get(call_id))
        .map(|trace| {
            json!({
                "call_id": trace.call_id,
                "started": trace.started,
                "hand": trace.hand,
                "tool": trace.tool,
                "input_summary": trace.input_summary,
                "status": trace.status.map(tool_status).unwrap_or("pending"),
                "duration_ms": trace.duration_ms,
                "output_summary": trace.output_summary,
                "artifact_refs": trace.artifact_refs,
                "error_code": trace.error_code,
            })
        })
        .collect();

    let tool_dangling = tool_calls
        .values()
        .filter(|trace| trace.started && trace.status.is_none())
        .count();
    let tool_orphan_completed = tool_calls
        .values()
        .filter(|trace| !trace.started && trace.status.is_some())
        .count();

    let mut fact_invalid_sources = 0usize;
    let mut fact_undigested_sources = 0usize;
    for item in &fact_checks {
        if let Some(sources) = item["sources"].as_array() {
            for source in sources {
                if let Some(url) = source.as_str() {
                    if !accepted_sources.contains(url) {
                        fact_invalid_sources += 1;
                    }
                    if !digested_sources.contains(url) {
                        fact_undigested_sources += 1;
                    }
                }
            }
        }
    }

    let mut blockers = Vec::new();
    if tool_dangling > 0 {
        blockers.push(format!("tool_calls_dangling {tool_dangling} > 0"));
    }
    if tools_error > 0 {
        blockers.push(format!("tool_call_errors {tools_error} > 0"));
    }
    if fact_check_required && fact_checks.is_empty() {
        blockers.push("fact_checks_total 0 < 1".to_string());
    }
    if fact_refuted > 0 {
        blockers.push(format!("fact_checks_refuted {fact_refuted} > 0"));
    }
    if fact_uncertain > 0 {
        blockers.push(format!("fact_checks_uncertain {fact_uncertain} > 0"));
    }
    if fact_invalid_sources > 0 {
        blockers.push(format!(
            "fact_check_invalid_sources {fact_invalid_sources} > 0"
        ));
    }
    if fact_undigested_sources > 0 {
        blockers.push(format!(
            "fact_check_undigested_sources {fact_undigested_sources} > 0"
        ));
    }
    if tool_orphan_completed > 0 {
        blockers.push(format!(
            "tool_calls_orphan_completed {tool_orphan_completed} > 0"
        ));
    }
    if synth_completed == 0 {
        blockers.push("synthesize_completed 0 < 1".to_string());
    }
    let report_html = inspect_report_html(&slug, latest_report_html.as_deref());
    let zh_paragraphs = report_html["zh_paragraphs"].as_u64().unwrap_or(0);
    if synth_bilingual_started > 0 && zh_paragraphs == 0 {
        blockers.push("bilingual_requested_but_no_zh_paragraphs".to_string());
    }

    let audit_status = if blockers.is_empty() {
        "complete"
    } else {
        "incomplete"
    };

    let timeline: Vec<Value> = events
        .iter()
        .enumerate()
        .map(|(idx, ev)| timeline_entry(idx + 1, ev))
        .collect();

    Envelope::ok(
        CMD,
        json!({
            "slug": cfg.slug,
            "topic": cfg.topic,
            "preset": cfg.preset,
            "tags": cfg.tags,
            "audit_status": audit_status,
            "audit_blockers": blockers,
            "events_total": events.len(),
            "sources": {
                "attempted": sources_attempted,
                "accepted": sources_accepted,
                "rejected": sources_rejected,
            },
            "tools": {
                "started": tools_started,
                "completed": tools_completed,
                "ok": tools_ok,
                "error": tools_error,
                "dangling": tool_dangling,
                "orphan_completed": tool_orphan_completed,
                "calls": tool_call_items,
            },
            "fact_checks": {
                "required": fact_check_required,
                "total": fact_checks.len(),
                "supported": fact_supported,
                "refuted": fact_refuted,
                "uncertain": fact_uncertain,
            "invalid_sources": fact_invalid_sources,
            "undigested_sources": fact_undigested_sources,
            "items": fact_checks,
        },
            "synthesis": {
                "started": synth_started,
                "completed": synth_completed,
                "failed": synth_failed,
                "report_json_path": latest_report_json,
                "report_html_path": latest_report_html,
                "latest_failure": latest_synth_failure,
                "report_html": report_html,
                "bilingual_requested": synth_bilingual_started > 0,
                "bilingual_started": synth_bilingual_started,
                "latest_bilingual_provider": latest_bilingual_provider,
            },
            "loop": {
                "started": loop_started,
                "steps": loop_steps,
                "completed": loop_completed,
                "last_reason": last_loop_reason,
                "last_report_ready": last_loop_report_ready,
            },
            "events": timeline,
        }),
    )
    .with_context(json!({ "session": slug }))
}

fn timeline_entry(index: usize, ev: &SessionEvent) -> Value {
    let raw = serde_json::to_value(ev).unwrap_or_else(|_| json!({}));
    json!({
        "index": index,
        "event": raw.get("event").and_then(Value::as_str).unwrap_or("unknown"),
        "timestamp": raw.get("timestamp").and_then(Value::as_str).unwrap_or(""),
        "summary": summarize_event(ev),
    })
}

fn summarize_event(ev: &SessionEvent) -> String {
    match ev {
        SessionEvent::SessionCreated { slug, topic, .. } => {
            format!("session created {slug}: {topic}")
        }
        SessionEvent::SourceAttempted {
            url,
            route_decision,
            ..
        } => format!(
            "source attempted via {} kind={} url={url}",
            route_decision.executor, route_decision.kind
        ),
        SessionEvent::SourceAccepted {
            url,
            kind,
            executor,
            bytes,
            ..
        } => format!("source accepted via {executor} kind={kind} bytes={bytes} url={url}"),
        SessionEvent::SourceRejected {
            url,
            reason,
            executor,
            ..
        } => format!("source rejected via {executor} reason={reason:?} url={url}"),
        SessionEvent::ToolCallStarted {
            call_id,
            hand,
            tool,
            ..
        } => format!("tool call started {call_id} hand={hand} tool={tool}"),
        SessionEvent::ToolCallCompleted {
            call_id,
            status,
            duration_ms,
            artifact_refs,
            ..
        } => format!(
            "tool call completed {call_id} status={} duration_ms={duration_ms} artifacts={}",
            tool_status(*status),
            artifact_refs.len()
        ),
        SessionEvent::FactChecked {
            claim,
            outcome,
            sources,
            ..
        } => format!(
            "fact checked outcome={} sources={} claim={}",
            fact_outcome(*outcome),
            sources.len(),
            claim
        ),
        SessionEvent::SynthesizeStarted {
            no_render,
            open,
            bilingual,
            bilingual_provider,
            ..
        } => format!(
            "synthesize started no_render={no_render} open={open} bilingual={bilingual} provider={}",
            bilingual_provider.as_deref().unwrap_or("none")
        ),
        SessionEvent::SynthesizeCompleted {
            report_html_path, ..
        } => format!(
            "synthesize completed html={}",
            report_html_path.as_deref().unwrap_or("none")
        ),
        SessionEvent::SynthesizeFailed { stage, reason, .. } => {
            format!("synthesize failed stage={stage:?} reason={reason}")
        }
        SessionEvent::LoopStarted {
            provider,
            iterations,
            max_actions,
            ..
        } => format!(
            "loop started provider={provider} iterations={iterations} max_actions={max_actions}"
        ),
        SessionEvent::LoopStep {
            iteration,
            actions_executed,
            actions_rejected,
            ..
        } => format!(
            "loop step iteration={iteration} actions_executed={actions_executed} actions_rejected={actions_rejected}"
        ),
        SessionEvent::LoopCompleted {
            reason,
            report_ready,
            ..
        } => format!("loop completed reason={reason} report_ready={report_ready}"),
        SessionEvent::SourceDigested {
            url, into_section, ..
        } => format!("source digested into={into_section} url={url}"),
        SessionEvent::PlanWritten { body_chars, .. } => {
            format!("plan written body_chars={body_chars}")
        }
        SessionEvent::DiagramAuthored { path, bytes, .. } => {
            format!("diagram authored path={path} bytes={bytes}")
        }
        SessionEvent::DiagramRejected { path, reason, .. } => {
            format!("diagram rejected path={path} reason={reason}")
        }
        SessionEvent::WikiPageWritten {
            slug,
            mode,
            body_chars,
            ..
        } => format!("wiki page written slug={slug} mode={mode} body_chars={body_chars}"),
        SessionEvent::SchemaUpdated { body_chars, .. } => {
            format!("schema updated body_chars={body_chars}")
        }
        SessionEvent::WikiQuery {
            question,
            relevant_pages,
            answer_slug,
            ..
        } => format!(
            "wiki query pages={} saved={} question={question}",
            relevant_pages.len(),
            answer_slug.as_deref().unwrap_or("none")
        ),
        SessionEvent::WikiLintRan {
            issues,
            orphans,
            broken_links,
            ..
        } => format!("wiki lint issues={issues} orphans={orphans} broken_links={broken_links}"),
        SessionEvent::SessionClosed { .. } => "session closed".to_string(),
        SessionEvent::SessionRemoved { .. } => "session removed".to_string(),
        SessionEvent::SessionResumed { .. } => "session resumed".to_string(),
    }
}

fn inspect_report_html(slug: &str, report_html_path: Option<&str>) -> Value {
    let Some(path) = report_html_path else {
        return json!({
            "exists": false,
            "path": null,
            "zh_paragraphs": 0,
            "language_switch": "absent",
        });
    };

    let resolved = resolve_report_path(slug, path);
    let Ok(html) = std::fs::read_to_string(&resolved) else {
        return json!({
            "exists": false,
            "path": resolved.display().to_string(),
            "zh_paragraphs": 0,
            "language_switch": "absent",
        });
    };

    let zh_paragraphs =
        html.matches(r#"class="tr-zh""#).count() + html.matches(r#"class='tr-zh'"#).count();
    let language_switch = if !html.contains(r#"class="lang-switch""#) {
        "absent"
    } else if html.contains(r#"data-mode="zh" disabled"#)
        || html.contains(r#"data-mode='zh' disabled"#)
    {
        "disabled"
    } else {
        "enabled"
    };

    json!({
        "exists": true,
        "path": resolved.display().to_string(),
        "zh_paragraphs": zh_paragraphs,
        "language_switch": language_switch,
    })
}

fn resolve_report_path(slug: &str, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    if p.components().next().and_then(|c| c.as_os_str().to_str()) == Some(slug) {
        layout::root_for_slug(slug).join(p)
    } else {
        layout::session_dir(slug).join(p)
    }
}

fn tool_status(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Ok => "ok",
        ToolCallStatus::Error => "error",
    }
}

fn fact_outcome(outcome: FactCheckOutcome) -> &'static str {
    match outcome {
        FactCheckOutcome::Supported => "supported",
        FactCheckOutcome::Refuted => "refuted",
        FactCheckOutcome::Uncertain => "uncertain",
    }
}
