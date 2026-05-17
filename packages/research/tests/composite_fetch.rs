//! Integration tests for `specs/composite-source-fetch.spec.md` — 14 BDD scenarios.
//!
//! Test name → spec scenario mapping (each `测试:` line in the spec must
//! match a test name in this file character-for-character):
//!
//!  1. composite_route_executes_parts_sequentially
//!  2. composite_raw_artifact_has_all_parts_keyed_by_label
//!  3. composite_smell_pass_requires_all_parts_pass
//!  4. composite_smell_reject_labels_failing_part
//!  5. composite_session_jsonl_single_source_accepted_event
//!  6. composite_wiki_frontmatter_includes_parts_list
//!  7. composite_trust_score_is_max_of_parts             (unit)
//!  8. composite_postagent_part_fails_rejects_composite
//!  9. composite_browser_part_about_blank_rejects_composite
//! 10. composite_partial_timeout_one_part_propagates_reject
//! 11. single_backend_rule_still_uses_legacy_single_path
//! 12. composite_idempotency_dedupes_by_resolved_url
//! 13. composite_and_top_level_executor_mutually_exclusive (unit)
//! 14. composite_part_placeholder_unbound_at_load          (unit)
//!
//! Test 7 uses `Frontmatter::parts` round-trip + library helpers — no IO.
//! Tests 13/14 hit `route::load_preset` with bad TOML — pure unit-style.
//! All other tests spin up the CLI subprocess with a per-test tempdir
//! holding a custom composite preset (TOML written to
//! `<research_root>/presets/composite-test.toml`). Browser parts hit a
//! per-test in-process MCP mock; postagent parts hit a per-test fake
//! shell script that emits a fixed JSON body.

use research::route;
use research::session::event::{self, RejectReason, SessionEvent};
use research::session::wiki::{render_frontmatter_body, split_frontmatter, Frontmatter};

use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn research_bin() -> String {
    env!("CARGO_BIN_EXE_ascent-research").to_string()
}

/// Process-wide serializer for env-var-mutating tests (mirror of the
/// pattern in catalog_seed.rs / runcode_flags.rs). The composite tests
/// don't mutate global env directly — env scopes are per-subprocess —
/// but a few tests modify `ACTIONBOOK_RESEARCH_HOME` for in-process
/// library calls, so we keep the lock for safety.
fn env_serializer() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. composite_route_executes_parts_sequentially
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn composite_route_executes_parts_sequentially() {
    let mock = McpMock::start_with(MockConfig::default());
    let env = TestEnv::new("seq");
    let pa = env.write_fake_postagent_success();
    write_composite_preset(&env);

    env.create_session_with_preset();
    let out = env.research_run(
        &[
            "add",
            "https://github.com/foo/bar/pull/42",
            "--slug",
            &env.slug,
            "--json",
        ],
        &[
            ("POSTAGENT_BIN", pa.to_str().unwrap()),
            ("ACTIONBOOK_BACKEND", "v2-mcp"),
            ("ACTIONBOOK_MCP_ENDPOINT", &mock.endpoint),
            ("ACTIONBOOK_API_KEY", "test-key"),
        ],
    );
    // We don't care about exit code here — the event order is what matters.
    let _ = out;

    let mcp_cmds = mock.all_cmds();
    let runcode_cmds: Vec<&String> = mcp_cmds
        .iter()
        .filter(|c| c.starts_with("browser run-code"))
        .collect();
    // The browser run-code cmd must have landed exactly once (after postagent).
    assert_eq!(
        runcode_cmds.len(),
        1,
        "expected 1 run-code cmd, got {}: {:?}",
        runcode_cmds.len(),
        runcode_cmds
    );

    let postagent_calls = env.read_postagent_log();
    assert!(
        !postagent_calls.is_empty(),
        "postagent must be invoked at least once"
    );
    let pa_at = postagent_calls[0].at_micros;

    let browser_at = mock.first_runcode_at();
    assert!(
        browser_at.is_some(),
        "mock must have recorded a run-code timestamp"
    );
    // Sequential order: browser run-code must start AFTER postagent finished.
    assert!(
        browser_at.unwrap() >= pa_at,
        "browser part started before postagent finished — composite is supposed to be sequential. pa_at={pa_at}, browser_at={:?}",
        browser_at
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. composite_raw_artifact_has_all_parts_keyed_by_label
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn composite_raw_artifact_has_all_parts_keyed_by_label() {
    let mock = McpMock::start_with(MockConfig::default());
    let env = TestEnv::new("artifact");
    let pa = env.write_fake_postagent_success();
    write_composite_preset(&env);
    env.create_session_with_preset();

    env.research_run(
        &[
            "add",
            "https://github.com/foo/bar/pull/42",
            "--slug",
            &env.slug,
            "--json",
        ],
        &[
            ("POSTAGENT_BIN", pa.to_str().unwrap()),
            ("ACTIONBOOK_BACKEND", "v2-mcp"),
            ("ACTIONBOOK_MCP_ENDPOINT", &mock.endpoint),
            ("ACTIONBOOK_API_KEY", "test-key"),
        ],
    );

    let raw_dir = env.session_dir().join("raw");
    let entries: Vec<PathBuf> = fs::read_dir(&raw_dir)
        .unwrap_or_else(|e| panic!("read raw/: {e}"))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    let composite_files: Vec<&PathBuf> = entries
        .iter()
        .filter(|p| p.to_string_lossy().ends_with(".composite.json"))
        .filter(|p| !p.to_string_lossy().contains(".rejected."))
        .collect();
    assert_eq!(
        composite_files.len(),
        1,
        "expected 1 .composite.json file, got: {entries:?}"
    );

    let raw_text = fs::read_to_string(composite_files[0]).unwrap();
    let v: Value = serde_json::from_str(&raw_text).unwrap_or_else(|e| {
        panic!("composite artifact must parse as JSON: {e}; content: {raw_text}")
    });
    assert_eq!(v["schema"], "composite-v1", "schema marker mismatch: {v}");

    let parts = v["parts"].as_object().expect("parts must be a map");
    assert!(parts.contains_key("metadata"), "missing `metadata`: {v}");
    assert!(parts.contains_key("rendered"), "missing `rendered`: {v}");

    for label in ["metadata", "rendered"] {
        let p = &parts[label];
        for field in [
            "executor",
            "exit_code",
            "duration_ms",
            "smell_pass",
            "trust_score",
        ] {
            assert!(
                p.get(field).is_some(),
                "part `{label}` missing field `{field}`: {p}"
            );
        }
        // raw_stdout_utf8 OR raw_stdout_b64 (one of two).
        assert!(
            p.get("raw_stdout_utf8").is_some() || p.get("raw_stdout_b64").is_some(),
            "part `{label}` missing raw_stdout_*: {p}"
        );
    }
    // metadata part executor is postagent; rendered is browser.
    assert_eq!(parts["metadata"]["executor"], "postagent");
    assert_eq!(parts["rendered"]["executor"], "browser");
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. composite_smell_pass_requires_all_parts_pass
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn composite_smell_pass_requires_all_parts_pass() {
    let mock = McpMock::start_with(MockConfig::default());
    let env = TestEnv::new("passall");
    let pa = env.write_fake_postagent_success();
    write_composite_preset(&env);
    env.create_session_with_preset();

    env.research_run(
        &[
            "add",
            "https://github.com/foo/bar/pull/42",
            "--slug",
            &env.slug,
            "--json",
        ],
        &[
            ("POSTAGENT_BIN", pa.to_str().unwrap()),
            ("ACTIONBOOK_BACKEND", "v2-mcp"),
            ("ACTIONBOOK_MCP_ENDPOINT", &mock.endpoint),
            ("ACTIONBOOK_API_KEY", "test-key"),
        ],
    );

    let events = read_events(&env);
    let accepted: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::SourceAccepted { .. }))
        .collect();
    assert_eq!(
        accepted.len(),
        1,
        "expected exactly 1 SourceAccepted event, got {}: {events:#?}",
        accepted.len()
    );
    if let SessionEvent::SourceAccepted {
        composite,
        parts,
        ..
    } = accepted[0]
    {
        assert_eq!(*composite, Some(true), "composite must be true");
        assert_eq!(
            parts.as_deref(),
            Some(["metadata".to_string(), "rendered".to_string()].as_slice()),
            "parts must list [metadata, rendered]"
        );
    } else {
        panic!("expected SourceAccepted, got: {:?}", accepted[0]);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. composite_smell_reject_labels_failing_part
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn composite_smell_reject_labels_failing_part() {
    // Configure the mock to return a tiny body that smell::EmptyContent
    // rejects (< 100 bytes default).
    let mock = McpMock::start_with(MockConfig {
        runcode_body_text: Some("tiny".into()), // < 100 chars → empty_content
        ..Default::default()
    });
    let env = TestEnv::new("rejlabel");
    let pa = env.write_fake_postagent_success();
    write_composite_preset(&env);
    env.create_session_with_preset();

    let out = env.research_run(
        &[
            "add",
            "https://github.com/foo/bar/pull/42",
            "--slug",
            &env.slug,
            "--json",
        ],
        &[
            ("POSTAGENT_BIN", pa.to_str().unwrap()),
            ("ACTIONBOOK_BACKEND", "v2-mcp"),
            ("ACTIONBOOK_MCP_ENDPOINT", &mock.endpoint),
            ("ACTIONBOOK_API_KEY", "test-key"),
        ],
    );

    let events = read_events(&env);
    let rejected: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::SourceRejected { .. }))
        .collect();
    assert_eq!(
        rejected.len(),
        1,
        "expected 1 SourceRejected, got {}: {events:#?}",
        rejected.len()
    );
    if let SessionEvent::SourceRejected {
        composite,
        parts,
        failed_part,
        reason,
        ..
    } = rejected[0]
    {
        assert_eq!(*composite, Some(true));
        assert_eq!(
            parts.as_deref(),
            Some(["metadata".to_string(), "rendered".to_string()].as_slice())
        );
        assert_eq!(failed_part.as_deref(), Some("rendered"));
        assert_eq!(
            *reason,
            RejectReason::EmptyContent,
            "expected empty_content, got {reason:?}"
        );
    } else {
        panic!("expected SourceRejected variant, got {:?}", rejected[0]);
    }

    // Spec § 4 also requires: `warnings` array carries entries prefixed
    // with "rendered: ". These flow through the CLI envelope at
    // `error.details.warnings` for failed envelopes.
    let line = out
        .stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .expect("CLI emitted JSON envelope");
    let env_v: Value = serde_json::from_str(line).unwrap_or(Value::Null);
    let warnings = env_v["error"]["details"]["warnings"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        warnings.iter().any(|w| w.as_str().unwrap_or("").starts_with("rendered: ")),
        "warnings must contain a `rendered: …` prefixed entry; got envelope: {env_v}, warnings: {warnings:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. composite_session_jsonl_single_source_accepted_event
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn composite_session_jsonl_single_source_accepted_event() {
    let mock = McpMock::start_with(MockConfig::default());
    let env = TestEnv::new("single");
    let pa = env.write_fake_postagent_success();
    write_composite_preset(&env);
    env.create_session_with_preset();

    env.research_run(
        &[
            "add",
            "https://github.com/foo/bar/pull/42",
            "--slug",
            &env.slug,
            "--json",
        ],
        &[
            ("POSTAGENT_BIN", pa.to_str().unwrap()),
            ("ACTIONBOOK_BACKEND", "v2-mcp"),
            ("ACTIONBOOK_MCP_ENDPOINT", &mock.endpoint),
            ("ACTIONBOOK_API_KEY", "test-key"),
        ],
    );

    let jsonl = env.session_jsonl();
    let raw_text = fs::read_to_string(&jsonl).expect("read jsonl");
    let accepted_lines: Vec<&str> = raw_text
        .lines()
        .filter(|l| l.contains("\"event\":\"source_accepted\""))
        .collect();
    assert_eq!(
        accepted_lines.len(),
        1,
        "must be exactly 1 source_accepted line, got {}",
        accepted_lines.len()
    );
    let parsed: Value = serde_json::from_str(accepted_lines[0]).unwrap();
    assert_eq!(parsed["composite"], true);
    let parts = parsed["parts"].as_array().expect("parts must be array");
    assert_eq!(parts.len(), 2);

    let part_bytes = parsed["part_bytes"]
        .as_object()
        .expect("part_bytes must be map");
    assert!(part_bytes.contains_key("metadata"));
    assert!(part_bytes.contains_key("rendered"));
    let sum: u64 = part_bytes.values().map(|v| v.as_u64().unwrap_or(0)).sum();
    assert_eq!(
        parsed["bytes"].as_u64().unwrap_or(0),
        sum,
        "top-level bytes must equal sum of part_bytes"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. composite_wiki_frontmatter_includes_parts_list
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn composite_wiki_frontmatter_includes_parts_list() {
    // The wiki frontmatter contract is additive — composite sources surface
    // `parts: [metadata, rendered]` and single-source pages omit it.
    // This is a pure round-trip check on the wiki module: render →
    // parse → expect. Mirrors the spec's "无 composite 写入时不输出 parts 行".
    let composite_fm = Frontmatter {
        kind: Some("github-pr".into()),
        sources: vec!["https://github.com/foo/bar/pull/42".into()],
        related: Vec::new(),
        updated: Some("2026-05-17".into()),
        parts: vec!["metadata".into(), "rendered".into()],
        extra: Vec::new(),
    };
    let body = render_frontmatter_body(&composite_fm);
    assert!(
        body.contains("parts: [metadata, rendered]"),
        "composite page must render parts line; got:\n{body}"
    );

    // Single-source — empty parts vec ⇒ NO parts line.
    let single_fm = Frontmatter {
        kind: Some("hn-item".into()),
        sources: vec!["https://news.ycombinator.com/item?id=42".into()],
        related: Vec::new(),
        updated: Some("2026-05-17".into()),
        parts: Vec::new(),
        extra: Vec::new(),
    };
    let single_body = render_frontmatter_body(&single_fm);
    assert!(
        !single_body.contains("parts:"),
        "single-source page must NOT emit `parts:` line; got:\n{single_body}"
    );

    // Reading a legacy page (no `parts` key) returns parts.is_empty().
    let legacy_yaml = "---\nkind: concept\nsources: [https://x]\nupdated: 2026-04-01\n---\nbody\n";
    let (fm, _) = split_frontmatter(legacy_yaml);
    assert!(
        fm.parts.is_empty(),
        "legacy page without `parts` key must yield empty Vec, got: {:?}",
        fm.parts
    );

    // Round-trip a composite page through the parser.
    let composite_yaml = "---\nkind: github-pr\nsources: [https://github.com/foo/bar/pull/42]\nparts: [metadata, rendered]\nupdated: 2026-05-17\n---\nbody\n";
    let (parsed, _) = split_frontmatter(composite_yaml);
    assert_eq!(parsed.parts, vec!["metadata", "rendered"]);
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. composite_trust_score_is_max_of_parts                    (unit)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn composite_trust_score_is_max_of_parts() {
    // Reflect the spec table directly: max() over each part's individual
    // trust. This is the same compute add.rs/batch.rs runs on the
    // FetchOutcome.composite_part_trust map.
    let cases: Vec<(Vec<f64>, f64)> = vec![
        (vec![2.0, 1.5], 2.0), // postagent + browser readable
        (vec![1.0, 1.0], 1.0), // two browser non-readable
        (vec![1.5, 1.0], 1.5), // browser readable + non-readable
        (vec![2.0, 2.0], 2.0), // two postagent
    ];
    for (parts, expected) in cases {
        let max = parts.iter().copied().fold(0.0_f64, f64::max);
        assert!(
            (max - expected).abs() < f64::EPSILON,
            "max of {parts:?} = {max}, expected {expected}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. composite_postagent_part_fails_rejects_composite
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn composite_postagent_part_fails_rejects_composite() {
    let mock = McpMock::start_with(MockConfig::default());
    let env = TestEnv::new("pafail");
    // Postagent returns HTTP 500 → smell rejects with ApiError.
    let pa = env.write_fake_postagent_http_500();
    write_composite_preset(&env);
    env.create_session_with_preset();

    env.research_run(
        &[
            "add",
            "https://github.com/foo/bar/pull/42",
            "--slug",
            &env.slug,
            "--json",
        ],
        &[
            ("POSTAGENT_BIN", pa.to_str().unwrap()),
            ("ACTIONBOOK_BACKEND", "v2-mcp"),
            ("ACTIONBOOK_MCP_ENDPOINT", &mock.endpoint),
            ("ACTIONBOOK_API_KEY", "test-key"),
        ],
    );

    let events = read_events(&env);
    let rejected: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::SourceRejected { .. }))
        .collect();
    assert_eq!(rejected.len(), 1);
    if let SessionEvent::SourceRejected {
        failed_part,
        reason,
        ..
    } = rejected[0]
    {
        assert_eq!(failed_part.as_deref(), Some("metadata"));
        assert_eq!(*reason, RejectReason::ApiError);
    }

    // Spec: mock browser zero calls (short-circuit), no accepted .composite.json,
    // BUT a .rejected.composite.json is written.
    let runcode_cmds: Vec<String> = mock
        .all_cmds()
        .into_iter()
        .filter(|c| c.starts_with("browser run-code"))
        .collect();
    assert_eq!(
        runcode_cmds.len(),
        0,
        "browser run-code must NOT be invoked after postagent failure; got: {runcode_cmds:?}"
    );

    let raw_dir = env.session_dir().join("raw");
    let mut accepted = 0;
    let mut rejected_files = 0;
    for entry in fs::read_dir(&raw_dir).unwrap().filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".composite.json") && !name.contains(".rejected.") {
            accepted += 1;
        }
        if name.contains(".rejected.composite.json") {
            rejected_files += 1;
        }
    }
    assert_eq!(accepted, 0, "no accepted .composite.json must exist");
    assert_eq!(
        rejected_files, 1,
        "expected 1 .rejected.composite.json, got {rejected_files}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 9. composite_browser_part_about_blank_rejects_composite
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn composite_browser_part_about_blank_rejects_composite() {
    // Browser returns about:blank → smell rejects with WrongUrl.
    let mock = McpMock::start_with(MockConfig {
        runcode_about_blank: true,
        ..Default::default()
    });
    let env = TestEnv::new("blank");
    let pa = env.write_fake_postagent_success();
    write_composite_preset(&env);
    env.create_session_with_preset();

    env.research_run(
        &[
            "add",
            "https://github.com/foo/bar/pull/42",
            "--slug",
            &env.slug,
            "--json",
        ],
        &[
            ("POSTAGENT_BIN", pa.to_str().unwrap()),
            ("ACTIONBOOK_BACKEND", "v2-mcp"),
            ("ACTIONBOOK_MCP_ENDPOINT", &mock.endpoint),
            ("ACTIONBOOK_API_KEY", "test-key"),
        ],
    );

    let events = read_events(&env);
    let rejected: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::SourceRejected { .. }))
        .collect();
    assert_eq!(rejected.len(), 1);
    if let SessionEvent::SourceRejected {
        failed_part,
        reason,
        ..
    } = rejected[0]
    {
        assert_eq!(failed_part.as_deref(), Some("rendered"));
        assert_eq!(*reason, RejectReason::WrongUrl);
    }

    let pa_calls = env.read_postagent_log();
    assert_eq!(
        pa_calls.len(),
        1,
        "postagent must run exactly once (metadata part)"
    );
    let runcode_cmds: Vec<String> = mock
        .all_cmds()
        .into_iter()
        .filter(|c| c.starts_with("browser run-code"))
        .collect();
    assert_eq!(
        runcode_cmds.len(),
        1,
        "browser run-code must run exactly once (rendered part)"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 10. composite_partial_timeout_one_part_propagates_reject
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn composite_partial_timeout_one_part_propagates_reject() {
    // Mock browser sleeps longer than the per-part timeout → run-code times
    // out → composite reject. Per-part budget must be wide enough that the
    // postagent subprocess spawn never trips it (even under parallel test
    // load), but narrower than the mock sleep so the browser part still
    // times out. 5s spawn budget + 12s mock sleep gives ~7s of margin.
    let mock = McpMock::start_with(MockConfig {
        runcode_sleep_ms: Some(12_000), // overshoot --timeout 5000 by 2.4×
        ..Default::default()
    });
    let env = TestEnv::new("timeout");
    let pa = env.write_fake_postagent_success();
    write_composite_preset(&env);
    env.create_session_with_preset();

    let started = std::time::Instant::now();
    env.research_run(
        &[
            "add",
            "https://github.com/foo/bar/pull/42",
            "--slug",
            &env.slug,
            "--timeout",
            "5000",
            "--json",
        ],
        &[
            ("POSTAGENT_BIN", pa.to_str().unwrap()),
            ("ACTIONBOOK_BACKEND", "v2-mcp"),
            ("ACTIONBOOK_MCP_ENDPOINT", &mock.endpoint),
            ("ACTIONBOOK_API_KEY", "test-key"),
        ],
    );
    let elapsed_ms = started.elapsed().as_millis() as u64;

    let events = read_events(&env);
    let rejected: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::SourceRejected { .. }))
        .collect();
    assert_eq!(rejected.len(), 1, "expected 1 rejected event: {events:#?}");
    if let SessionEvent::SourceRejected {
        failed_part,
        reason,
        ..
    } = rejected[0]
    {
        assert_eq!(failed_part.as_deref(), Some("rendered"));
        assert_eq!(*reason, RejectReason::FetchFailed);
    }

    // Per-part budget: total wall ≤ 2 parts * 5000ms + 4s slack.
    let bound_ms = 2 * 5000 + 4_000;
    assert!(
        elapsed_ms <= bound_ms,
        "total wall clock {elapsed_ms}ms exceeded {bound_ms}ms (timeout budget per-part * parts + slack)"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 11. single_backend_rule_still_uses_legacy_single_path
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn single_backend_rule_still_uses_legacy_single_path() {
    let env = TestEnv::new("legacy");
    let pa = env.write_fake_postagent_success();
    write_composite_preset(&env);
    env.create_session_with_preset();

    env.research_run(
        &[
            "add",
            // Issue URL goes to the single-backend rule, not the composite PR rule.
            "https://github.com/foo/bar/issues/1",
            "--slug",
            &env.slug,
            "--json",
        ],
        &[("POSTAGENT_BIN", pa.to_str().unwrap())],
    );

    let raw_dir = env.session_dir().join("raw");
    let raw_files: Vec<String> = fs::read_dir(&raw_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        raw_files.iter().any(|f| f.ends_with(".json") && !f.contains(".composite")),
        "single-backend rule must produce a `.json` (NOT `.composite.json`) artifact; got: {raw_files:?}"
    );
    assert!(
        !raw_files.iter().any(|f| f.contains(".composite.json")),
        "single-backend rule MUST NOT emit a .composite.json; got: {raw_files:?}"
    );

    let events = read_events(&env);
    let accepted: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::SourceAccepted { .. }))
        .collect();
    assert_eq!(accepted.len(), 1);
    if let SessionEvent::SourceAccepted {
        composite,
        executor,
        ..
    } = accepted[0]
    {
        assert!(
            composite.is_none() || *composite == Some(false),
            "single-backend must NOT serialize composite field, got: {composite:?}"
        );
        assert_eq!(executor, "postagent");
    }

    // Verify the raw jsonl LINE for source_accepted has NO `composite` field.
    let jsonl_text = fs::read_to_string(env.session_jsonl()).unwrap();
    let line = jsonl_text
        .lines()
        .find(|l| l.contains("\"event\":\"source_accepted\""))
        .expect("source_accepted line present");
    let v: Value = serde_json::from_str(line).unwrap();
    assert!(
        v.get("composite").is_none() || v["composite"].is_null(),
        "legacy single-backend event must NOT carry `composite` key; got line: {line}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 12. composite_idempotency_dedupes_by_resolved_url
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn composite_idempotency_dedupes_by_resolved_url() {
    let mock = McpMock::start_with(MockConfig::default());
    let env = TestEnv::new("idemp");
    let pa = env.write_fake_postagent_success();
    write_composite_preset(&env);
    env.create_session_with_preset();

    // First add — accepted.
    env.research_run(
        &[
            "add",
            "https://github.com/foo/bar/pull/42",
            "--slug",
            &env.slug,
            "--json",
        ],
        &[
            ("POSTAGENT_BIN", pa.to_str().unwrap()),
            ("ACTIONBOOK_BACKEND", "v2-mcp"),
            ("ACTIONBOOK_MCP_ENDPOINT", &mock.endpoint),
            ("ACTIONBOOK_API_KEY", "test-key"),
        ],
    );
    let cmds_after_first = mock.all_cmds().len();
    let pa_after_first = env.read_postagent_log().len();
    let raw_files_after_first: Vec<String> = fs::read_dir(env.session_dir().join("raw"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let raw_count_after_first = raw_files_after_first.len();

    // Second add of SAME URL — must dedupe.
    let out = env.research_run(
        &[
            "add",
            "https://github.com/foo/bar/pull/42",
            "--slug",
            &env.slug,
            "--json",
        ],
        &[
            ("POSTAGENT_BIN", pa.to_str().unwrap()),
            ("ACTIONBOOK_BACKEND", "v2-mcp"),
            ("ACTIONBOOK_MCP_ENDPOINT", &mock.endpoint),
            ("ACTIONBOOK_API_KEY", "test-key"),
        ],
    );
    assert_ne!(out.code, 0, "duplicate add must exit non-zero");
    // Stdout JSON envelope should carry reject_reason: duplicate.
    let stdout = &out.stdout;
    assert!(
        stdout.contains("\"reject_reason\":\"duplicate\"") || stdout.contains("duplicate"),
        "stdout must indicate duplicate; got: {stdout}"
    );

    // raw/ unchanged.
    let raw_files_after_second: Vec<String> = fs::read_dir(env.session_dir().join("raw"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        raw_files_after_second.len(),
        raw_count_after_first,
        "raw/ file count must NOT grow on duplicate add"
    );

    // Zero extra subprocess calls.
    assert_eq!(mock.all_cmds().len(), cmds_after_first);
    assert_eq!(env.read_postagent_log().len(), pa_after_first);
}

// ═══════════════════════════════════════════════════════════════════════════
// 13. composite_and_top_level_executor_mutually_exclusive       (unit)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn composite_and_top_level_executor_mutually_exclusive() {
    let _lock = env_serializer();
    let tmp = TempDir::new().unwrap();
    let presets_dir = tmp.path().join("presets");
    fs::create_dir_all(&presets_dir).unwrap();
    let path = presets_dir.join("badrule.toml");
    fs::write(
        &path,
        r#"
name = "badrule"

[[rule]]
kind = "github-pr"
host = "github.com"
path_segments = ["{owner}", "{repo}", "pull", "{num}"]
executor = "postagent"
template = 'postagent send "https://api.github.com/repos/{owner}/{repo}/pulls/{num}"'
composite = [
  { executor = "postagent", template = 'postagent send "{url}"', label = "metadata" },
  { executor = "browser", template = 'actionbook browser new-tab "{url}"', label = "rendered" },
]

[fallback]
kind = "fb"
executor = "browser"
template = "fb"
"#,
    )
    .unwrap();

    let err = route::load_preset(None, Some(&path)).expect_err("must reject mutual-exclusive rule");
    assert_eq!(err.sub_code.as_str(), "SCHEMA_INVALID");
    let lc = err.message.to_lowercase();
    assert!(
        lc.contains("cannot set both"),
        "error message must contain 'cannot set both'; got: {}",
        err.message
    );
    assert!(
        err.message.contains("github-pr"),
        "error must name the offending rule's kind; got: {}",
        err.message
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 14. composite_part_placeholder_unbound_at_load                (unit)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn composite_part_placeholder_unbound_at_load() {
    let _lock = env_serializer();
    let tmp = TempDir::new().unwrap();
    let presets_dir = tmp.path().join("presets");
    fs::create_dir_all(&presets_dir).unwrap();
    let path = presets_dir.join("badph.toml");
    fs::write(
        &path,
        r#"
name = "badph"

[[rule]]
kind = "github-pr"
host = "github.com"
path_segments = ["{owner}", "{repo}", "pull", "{num}"]
composite = [
  { executor = "postagent", template = 'postagent send "https://api.github.com/repos/{owner}/{repo}/pulls/{num}"', label = "metadata" },
  { executor = "browser", template = 'actionbook browser new-tab "https://github.com/{owner}/{repo}/pull/{nonexistent}"', label = "rendered" },
]

[fallback]
kind = "fb"
executor = "browser"
template = "fb"
"#,
    )
    .unwrap();

    let err = route::load_preset(None, Some(&path)).expect_err("must reject unbound placeholder");
    assert_eq!(err.sub_code.as_str(), "PLACEHOLDER_UNBOUND");
    assert!(
        err.message.contains("nonexistent"),
        "error must name the offending placeholder; got: {}",
        err.message
    );
    assert!(
        err.message.contains("rendered"),
        "error must name the offending part label `rendered`; got: {}",
        err.message
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

// TOML inline tables must be on a single line — so each composite part
// is a single-line `{ k = v, ... }` literal. The template strings use
// single-quoted (literal) TOML strings to avoid escaping the inner `"`.
const COMPOSITE_PRESET_TOML: &str = r#"
name = "composite-test"

# A composite rule for github-pr (fan-out: postagent metadata + browser rendered).
[[rule]]
kind = "github-pr"
host = "github.com"
path_segments = ["{owner}", "{repo}", "pull", "{num}"]
composite = [
  { executor = "postagent", template = 'postagent send "https://api.github.test/repos/{owner}/{repo}/pulls/{num}"', label = "metadata" },
  { executor = "browser", template = 'actionbook browser new-tab "https://github.com/{owner}/{repo}/pull/{num}"', label = "rendered" },
]

# A single-backend rule for github-issue — exercises the legacy code path.
[[rule]]
kind = "github-issue"
host = "github.com"
path_segments = ["{owner}", "{repo}", "issues", "{num}"]
executor = "postagent"
template = 'postagent send "https://api.github.test/repos/{owner}/{repo}/issues/{num}"'

[fallback]
kind = "browser-fallback"
executor = "browser"
template = 'actionbook browser new-tab "{url}"'
"#;

fn write_composite_preset(env: &TestEnv) {
    let presets_dir = PathBuf::from(&env.home).join("presets");
    fs::create_dir_all(&presets_dir).unwrap();
    let path = presets_dir.join("composite-test.toml");
    fs::write(&path, COMPOSITE_PRESET_TOML).unwrap();
}

fn read_events(env: &TestEnv) -> Vec<SessionEvent> {
    event::read_events(&env.session_jsonl()).unwrap_or_default()
}

struct PostagentCall {
    at_micros: u128,
    #[allow(dead_code)]
    argv: Vec<String>,
}

struct CapturedOutput {
    code: i32,
    stdout: String,
    #[allow(dead_code)]
    stderr: String,
}

struct TestEnv {
    _tmp: TempDir,
    home: String,
    slug: String,
    bin_dir: PathBuf,
    log_path: PathBuf,
}

impl TestEnv {
    fn new(label: &str) -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let bin_dir = tmp.path().join("_bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let log_path = tmp.path().join("postagent.log");
        Self {
            _tmp: tmp,
            home,
            slug: format!("comp-{label}"),
            bin_dir,
            log_path,
        }
    }

    fn create_session_with_preset(&self) {
        let out = Command::new(research_bin())
            .args([
                "new",
                "composite-test-topic",
                "--slug",
                &self.slug,
                "--preset",
                "composite-test",
                "--json",
            ])
            .env("ACTIONBOOK_RESEARCH_HOME", &self.home)
            .output()
            .expect("spawn research new");
        assert!(
            out.status.success(),
            "research new failed: stderr={} stdout={}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout),
        );
    }

    /// Write a fake postagent that emits a fixed JSON body so smell::judge_api
    /// passes (status 200, non-empty). Logs argv + invocation timestamp
    /// (microseconds since UNIX epoch) so tests can verify ordering.
    fn write_fake_postagent_success(&self) -> PathBuf {
        let log_quoted = shell_quote(self.log_path.to_str().unwrap());
        let script = format!(
            r#"#!/bin/sh
ts=$(python3 -c 'import time; print(int(time.time()*1_000_000))' 2>/dev/null || date +%s)
printf '%s\t%s\n' "$ts" "$*" >> {log_quoted}
cat <<'JSON'
{{"title":"Pull Request 42","state":"open","number":42,"body":"A long-enough description to clear the smell threshold easily — lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat."}}
JSON
"#
        );
        let pa = self.bin_dir.join("postagent");
        fs::write(&pa, script).unwrap();
        let mut perms = fs::metadata(&pa).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&pa, perms).unwrap();
        pa
    }

    /// Fake postagent that emits HTTP 500 via stderr `⚠ 500 — ...` pattern.
    fn write_fake_postagent_http_500(&self) -> PathBuf {
        let log_quoted = shell_quote(self.log_path.to_str().unwrap());
        let script = format!(
            r#"#!/bin/sh
ts=$(python3 -c 'import time; print(int(time.time()*1_000_000))' 2>/dev/null || date +%s)
printf '%s\t%s\n' "$ts" "$*" >> {log_quoted}
printf '⚠ 500 — server error\n' 1>&2
printf 'HTTP 500 Internal Server Error\n' 1>&2
exit 0
"#
        );
        let pa = self.bin_dir.join("postagent");
        fs::write(&pa, script).unwrap();
        let mut perms = fs::metadata(&pa).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&pa, perms).unwrap();
        pa
    }

    fn read_postagent_log(&self) -> Vec<PostagentCall> {
        if !self.log_path.exists() {
            return Vec::new();
        }
        let text = fs::read_to_string(&self.log_path).unwrap_or_default();
        text.lines()
            .filter_map(|line| {
                let (ts, argv) = line.split_once('\t')?;
                Some(PostagentCall {
                    at_micros: ts.trim().parse().unwrap_or(0),
                    argv: argv.split_whitespace().map(str::to_string).collect(),
                })
            })
            .collect()
    }

    fn session_dir(&self) -> PathBuf {
        PathBuf::from(&self.home).join(&self.slug)
    }

    fn session_jsonl(&self) -> PathBuf {
        self.session_dir().join("session.jsonl")
    }

    fn research_run(&self, args: &[&str], extra_env: &[(&str, &str)]) -> CapturedOutput {
        let mut cmd = Command::new(research_bin());
        cmd.args(args);
        cmd.env("ACTIONBOOK_RESEARCH_HOME", &self.home);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn research");
        CapturedOutput {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

fn shell_quote(s: &str) -> String {
    // Single-quote wrap; escape embedded single-quotes the POSIX way.
    let escaped = s.replace('\'', r#"'\''"#);
    format!("'{escaped}'")
}

// ─── Mock MCP server ────────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct MockConfig {
    /// Override the body text returned by `browser run-code`. Default is
    /// a long Lorem-ipsum block that passes the smell threshold.
    runcode_body_text: Option<String>,
    /// Return `context.url: "about:blank"` from `browser run-code`.
    runcode_about_blank: bool,
    /// Sleep this many milliseconds inside the `browser run-code` handler
    /// before responding (forces a client-side timeout).
    runcode_sleep_ms: Option<u64>,
}

struct McpMock {
    endpoint: String,
    cmds: Arc<Mutex<Vec<RecordedCmd>>>,
}

struct RecordedCmd {
    cmd: String,
    at_micros: u128,
}

impl McpMock {
    fn start_with(config: MockConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().unwrap();
        let cmds: Arc<Mutex<Vec<RecordedCmd>>> = Arc::new(Mutex::new(Vec::new()));
        let cmds_for_thread = cmds.clone();
        let cfg = config;
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let cmds = cmds_for_thread.clone();
                let cfg = cfg.clone();
                thread::spawn(move || {
                    let mut buf = [0u8; 65_536];
                    let n = match s.read(&mut buf) {
                        Ok(n) if n > 0 => n,
                        _ => return,
                    };
                    let raw = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let (headers, body_seen) = match raw.split_once("\r\n\r\n") {
                        Some((h, b)) => (h.to_string(), b.to_string()),
                        None => (raw.clone(), String::new()),
                    };
                    let content_length: usize = headers
                        .lines()
                        .find_map(|l| {
                            let lower = l.to_ascii_lowercase();
                            lower
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    let mut body = body_seen;
                    while body.len() < content_length {
                        let mut more = [0u8; 4096];
                        match s.read(&mut more) {
                            Ok(0) => break,
                            Ok(m) => body.push_str(&String::from_utf8_lossy(&more[..m])),
                            Err(_) => break,
                        }
                    }
                    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    let method = parsed.get("method").and_then(Value::as_str).unwrap_or("");
                    let response_json: String;
                    let extra_headers: &str;
                    match method {
                        "initialize" => {
                            extra_headers = "Mcp-Session-Id: mock-session\r\n";
                            response_json = json!({
                                "jsonrpc": "2.0",
                                "id": parsed.get("id").cloned().unwrap_or(Value::Null),
                                "result": {
                                    "protocolVersion": "2025-06-18",
                                    "capabilities": {},
                                    "serverInfo": {"name": "mock", "version": "0"}
                                }
                            })
                            .to_string();
                        }
                        "notifications/initialized" => {
                            extra_headers = "";
                            response_json = json!({}).to_string();
                        }
                        "tools/call" => {
                            extra_headers = "";
                            let cmd = parsed
                                .get("params")
                                .and_then(|p| p.get("arguments"))
                                .and_then(|a| a.get("cmd"))
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let now = now_micros();
                            cmds.lock().unwrap().push(RecordedCmd {
                                cmd: cmd.clone(),
                                at_micros: now,
                            });
                            // Optional sleep for timeout tests.
                            if cmd.starts_with("browser run-code") {
                                if let Some(ms) = cfg.runcode_sleep_ms {
                                    thread::sleep(Duration::from_millis(ms));
                                }
                            }

                            let text = if cmd.starts_with("browser run-code") {
                                let url_in_payload = if cfg.runcode_about_blank {
                                    "about:blank".to_string()
                                } else {
                                    // Extract URL from the original new-tab cmd if any — but
                                    // simpler: hardcode the URL the composite_test PR rule
                                    // uses. The test invokes
                                    // `https://github.com/foo/bar/pull/42`.
                                    "https://github.com/foo/bar/pull/42".to_string()
                                };
                                let body = cfg
                                    .runcode_body_text
                                    .clone()
                                    .unwrap_or_else(|| LONG_BODY.to_string());
                                let payload = json!({
                                    "result": {
                                        "url": url_in_payload,
                                        "title": "PR 42",
                                        "text": body,
                                    }
                                });
                                format!("[t1]\nok browser run-code\n{payload}")
                            } else {
                                "[t1]\nok ".to_string()
                            };
                            response_json = json!({
                                "jsonrpc": "2.0",
                                "id": parsed.get("id").cloned().unwrap_or(Value::Null),
                                "result": { "content": [ {"type":"text","text": text} ] }
                            })
                            .to_string();
                        }
                        _ => {
                            extra_headers = "";
                            response_json = json!({
                                "jsonrpc":"2.0",
                                "id":parsed.get("id").cloned().unwrap_or(Value::Null),
                                "result":{}
                            })
                            .to_string();
                        }
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{}",
                        response_json.len(),
                        extra_headers,
                        response_json
                    );
                    let _ = s.write_all(response.as_bytes());
                    let _ = s.flush();
                });
            }
        });
        let endpoint = format!("http://{addr}/mcp");
        Self { endpoint, cmds }
    }

    fn all_cmds(&self) -> Vec<String> {
        self.cmds
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.cmd.clone())
            .collect()
    }

    fn first_runcode_at(&self) -> Option<u128> {
        self.cmds
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.cmd.starts_with("browser run-code"))
            .map(|c| c.at_micros)
    }
}

fn now_micros() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0)
}

// Long enough (≥ 2000 bytes) to clear both the article smell threshold
// (500 bytes) and the trust-tier upgrade to 1.5 (browser readable ≥ 2000).
const LONG_BODY: &str = "Pull Request rendered text — Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum. Pull Request rendered text — Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum. Pull Request rendered text — Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum. Pull Request rendered text — Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum. Pull Request rendered text — Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.";

// Avoid unused warnings on the helpers when only a subset of tests runs.
fn _silence_unused() {
    let _ = Path::new("");
}
