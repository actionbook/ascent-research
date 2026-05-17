//! Integration tests for `autoresearch-actionbook-tools.spec.md` — 14 BDD
//! scenarios giving the LLM direct access to the V2 actionbook MCP tools
//! (`search`, `manual`, `run-code`) mid-loop.
//!
//! Test name → spec scenario mapping (each `测试:` line must match a test
//! name in this file character-for-character):
//!
//!  1. actionbook_search_action_dispatches_mcp_call
//!  2. actionbook_manual_action_seeds_wiki
//!  3. actionbook_runcode_action_returns_text_to_llm_context
//!  4. actionbook_runcode_truncates_at_16kb
//!  5. actionbook_runcode_per_loop_cap_3
//!  6. actionbook_search_per_loop_cap_5
//!  7. actionbook_action_fail_soft_on_extension_offline
//!  8. actionbook_action_fail_soft_on_api_key_missing
//!  9. actionbook_action_dry_run_skips_execution
//! 10. actionbook_unknown_action_field_rejected_in_response_parse        (unit)
//! 11. actionbook_action_logs_to_session_jsonl
//! 12. actionbook_manual_dedupe_with_existing_wiki_page
//! 13. actionbook_runcode_timeout_clamped_to_60s                          (unit)
//! 14. actionbook_action_v1_backend_skips
//!
//! Tests 1-9, 11, 12, 14 are integration: they spin up an in-process
//! mock MCP server (same pattern as `catalog_seed.rs` / `runcode_flags.rs`)
//! and drive the executor end-to-end with a `FakeProvider`. Tests 10 and
//! 13 are pure-unit and exercise the schema / clamp directly.

#![cfg(feature = "autoresearch")]

use async_trait::async_trait;
use research::autoresearch::executor::{self, LoopConfig};
use research::autoresearch::provider::{AgentProvider, FakeProvider, ProviderError};
use research::autoresearch::schema::{Action, LoopResponse};
use research::session::event::{self, SessionEvent};

use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use tempfile::TempDir;

fn research_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ascent-research"))
}

/// Process-wide serializer — same rationale as `catalog_seed.rs`: env-var
/// scoped guards (ACTIONBOOK_*) plus the global FS state demand we run
/// these one at a time so they don't stomp each other.
fn env_serializer() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

// ══════════════════════════════════════════════════════════════════════════
// 1. actionbook_search_action_dispatches_mcp_call
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn actionbook_search_action_dispatches_mcp_call() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig {
        search_hits: vec![
            hit("x_com", Some("search"), Some("search_timeline")),
            hit("x_com", Some("user"), Some("profile")),
        ],
        ..Default::default()
    });
    let env = TestEnv::new("ab1");
    let _envguard = mock.bind_env();

    let r1 = response_with_actions(
        "issue search",
        vec![json!({
            "type": "actionbook_search",
            "query": "tweet timeline",
            "host": "x.com",
        })],
        false,
    );
    let r2 = done_response("done");
    let provider = FakeProvider::new([r1, r2]);
    let report = run_loop(&provider, &env);

    let search_cmds = mock.search_cmds();
    assert_eq!(search_cmds.len(), 1, "expect exactly 1 search MCP call");
    assert!(
        search_cmds[0]
            .starts_with("actionbook search \"tweet timeline\" --host x.com"),
        "unexpected cmd: {}",
        search_cmds[0],
    );

    let events = event::read_events(&env.session_jsonl()).unwrap();
    let calls: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::ActionbookCalled { .. }))
        .collect();
    assert_eq!(calls.len(), 1, "exactly 1 ActionbookCalled event");
    if let SessionEvent::ActionbookCalled {
        action_type,
        outcome,
        ..
    } = calls[0]
    {
        assert_eq!(action_type, "actionbook_search");
        assert_eq!(outcome, "ok");
    }
    let _ = report;
}

// ══════════════════════════════════════════════════════════════════════════
// 2. actionbook_manual_action_seeds_wiki
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn actionbook_manual_action_seeds_wiki() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig {
        manual_body: "MANUAL-BODY-002".to_string(),
        ..Default::default()
    });
    let env = TestEnv::new("ab2");
    let _envguard = mock.bind_env();

    let r1 = response_with_actions(
        "pull manual",
        vec![json!({
            "type": "actionbook_manual",
            "site": "x_com",
            "group": "search",
            "action": "search_timeline",
        })],
        false,
    );
    let r2 = done_response("done");
    let provider = FakeProvider::new([r1, r2]);
    let _report = run_loop(&provider, &env);

    let page = env.wiki_dir().join("x-com-search-search-timeline.md");
    assert!(page.exists(), "wiki page must be seeded");
    let content = fs::read_to_string(&page).unwrap();
    assert!(content.contains("kind: actionbook-manual"), "fm kind missing: {content}");
    assert!(content.contains("source: catalog"), "fm source missing: {content}");
    assert!(content.contains("MANUAL-BODY-002"), "body missing: {content}");

    let events = event::read_events(&env.session_jsonl()).unwrap();
    let ab_event = events
        .iter()
        .find(|e| matches!(e, SessionEvent::ActionbookCalled { action_type, .. } if action_type == "actionbook_manual"))
        .expect("ActionbookCalled event present");
    if let SessionEvent::ActionbookCalled {
        wiki_seeded_pages,
        outcome,
        ..
    } = ab_event
    {
        assert_eq!(outcome, "ok");
        assert_eq!(
            wiki_seeded_pages.as_slice(),
            &["x-com-search-search-timeline".to_string()],
        );
    }
}

// ══════════════════════════════════════════════════════════════════════════
// 3. actionbook_runcode_action_returns_text_to_llm_context
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn actionbook_runcode_action_returns_text_to_llm_context() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig {
        runcode_payload: Some(json!({
            "url": "https://example.com/",
            "title": "Example",
            "text": "ABC",
        })),
        ..Default::default()
    });
    let env = TestEnv::new("ab3");
    let _envguard = mock.bind_env();

    let r1 = response_with_actions(
        "scrape",
        vec![json!({
            "type": "actionbook_run_code",
            "url": "https://example.com/",
            "script": "async (page) => ({ text: 'ABC' })",
        })],
        false,
    );
    let r2 = done_response("done");
    let capture = CapturingProvider::new([r1, r2]);
    let prompts = capture.prompts();
    let _report = run_loop(&capture, &env);

    let captured = prompts.lock().unwrap();
    assert!(captured.len() >= 2, "expect at least 2 turns of prompts");
    let second_user_prompt = &captured[1];
    assert!(
        second_user_prompt.contains("ABC"),
        "second turn must contain 'ABC' from run-code result:\n{second_user_prompt}",
    );
    assert!(
        second_user_prompt.contains("recent_actionbook_results"),
        "second turn must contain `recent_actionbook_results` block",
    );
    assert!(
        second_user_prompt.contains("\"action_type\": \"actionbook_run_code\""),
        "second turn must list action_type actionbook_run_code:\n{second_user_prompt}",
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 4. actionbook_runcode_truncates_at_16kb
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn actionbook_runcode_truncates_at_16kb() {
    let _lock = env_serializer();
    let big = "x".repeat(20 * 1024); // 20 KB raw text
    let mock = McpMock::start_with(MockConfig {
        runcode_payload: Some(json!({
            "url": "https://big.test/",
            "title": "Big",
            "text": big,
        })),
        ..Default::default()
    });
    let env = TestEnv::new("ab4");
    let _envguard = mock.bind_env();

    let r1 = response_with_actions(
        "scrape big",
        vec![json!({
            "type": "actionbook_run_code",
            "url": "https://big.test/",
            "script": "async (page) => ({ text: '...' })",
        })],
        false,
    );
    let r2 = done_response("done");
    let capture = CapturingProvider::new([r1, r2]);
    let prompts = capture.prompts();
    let _report = run_loop(&capture, &env);

    let captured = prompts.lock().unwrap();
    let second_user_prompt = &captured[1];

    // Pull just the text field's JSON-quoted value out of the recent_actionbook_results.
    // The injected text appears as `"text": "...x...[…truncated to 16KB…]"`.
    let injected_text = extract_injected_text(second_user_prompt)
        .expect("injected text payload should be present");
    assert!(
        injected_text.len() <= 16 * 1024,
        "injected text exceeds 16 KB cap: {} bytes",
        injected_text.len(),
    );
    assert!(
        injected_text.ends_with("[…truncated to 16KB…]"),
        "expected truncation marker at end, got: ...{}",
        &injected_text[injected_text.len().saturating_sub(40)..],
    );

    // session.jsonl event has result_truncated: true.
    let events = event::read_events(&env.session_jsonl()).unwrap();
    let ab = events
        .iter()
        .find_map(|e| match e {
            SessionEvent::ActionbookCalled {
                action_type,
                result_truncated,
                ..
            } if action_type == "actionbook_run_code" => Some(*result_truncated),
            _ => None,
        })
        .expect("ActionbookCalled event present");
    assert!(ab, "ActionbookCalled event must have result_truncated: true");
}

// ══════════════════════════════════════════════════════════════════════════
// 5. actionbook_runcode_per_loop_cap_3
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn actionbook_runcode_per_loop_cap_3() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig {
        runcode_payload: Some(json!({"text": "ok"})),
        ..Default::default()
    });
    let env = TestEnv::new("ab5");
    let _envguard = mock.bind_env();

    let actions: Vec<Value> = (0..4)
        .map(|i| {
            json!({
                "type": "actionbook_run_code",
                "url": format!("https://{i}.test/"),
                "script": "async (p) => ({})",
            })
        })
        .collect();
    let r1 = response_with_actions("flood runcode", actions, false);
    let r2 = done_response("done");
    let provider = FakeProvider::new([r1, r2]);
    let report = run_loop(&provider, &env);

    let runcode_cmds = mock.runcode_cmds();
    assert_eq!(
        runcode_cmds.len(),
        3,
        "expect exactly 3 run-code MCP RPCs (cap = 3), got: {}",
        runcode_cmds.len()
    );
    assert!(
        report.actions_rejected >= 1,
        "expect at least 1 rejected action, got: {}",
        report.actions_rejected
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("actionbook_per_loop_cap_exceeded")),
        "warnings must mention cap exceeded: {:?}",
        report.warnings
    );
    assert!(
        matches!(
            report.termination_reason,
            executor::TerminationReason::ProviderDone
                | executor::TerminationReason::IterationsExhausted
                | executor::TerminationReason::ReportReady
        ),
        "loop must not abort on cap exceed, got: {:?}",
        report.termination_reason,
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 6. actionbook_search_per_loop_cap_5
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn actionbook_search_per_loop_cap_5() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig {
        search_hits: vec![hit("x_com", None, None)],
        ..Default::default()
    });
    let env = TestEnv::new("ab6");
    let _envguard = mock.bind_env();

    let actions: Vec<Value> = (0..6)
        .map(|i| {
            json!({
                "type": "actionbook_search",
                "query": format!("query-{i}"),
            })
        })
        .collect();
    let r1 = response_with_actions("flood search", actions, false);
    let r2 = done_response("done");
    let provider = FakeProvider::new([r1, r2]);
    let report = run_loop(&provider, &env);

    let search_cmds = mock.search_cmds();
    assert_eq!(
        search_cmds.len(),
        5,
        "expect exactly 5 search MCP RPCs (cap = 5), got: {}",
        search_cmds.len()
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("actionbook_per_loop_cap_exceeded")),
        "warnings must mention cap exceeded: {:?}",
        report.warnings
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 7. actionbook_action_fail_soft_on_extension_offline
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn actionbook_action_fail_soft_on_extension_offline() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig {
        search_error_code: Some("EXTENSION_OFFLINE".to_string()),
        ..Default::default()
    });
    let env = TestEnv::new("ab7");
    let _envguard = mock.bind_env();

    let r1 = response_with_actions(
        "try search",
        vec![json!({
            "type": "actionbook_search",
            "query": "anything",
            "host": "x.com",
        })],
        false,
    );
    let r2 = done_response("done");
    let capture = CapturingProvider::new([r1, r2]);
    let prompts = capture.prompts();
    let report = run_loop(&capture, &env);

    assert_eq!(report.iterations_run, 2, "loop must complete both iters");

    let captured = prompts.lock().unwrap();
    let second_user_prompt = &captured[1];
    assert!(
        second_user_prompt.contains("recent_actionbook_results"),
        "iter 2 prompt must include recent_actionbook_results"
    );
    assert!(
        second_user_prompt.contains("chrome extension offline"),
        "iter 2 prompt must mention 'chrome extension offline':\n{second_user_prompt}",
    );
    assert!(
        second_user_prompt.contains("\"recoverable\": true"),
        "iter 2 prompt must include recoverable: true:\n{second_user_prompt}",
    );

    let events = event::read_events(&env.session_jsonl()).unwrap();
    let ab = events
        .iter()
        .find_map(|e| match e {
            SessionEvent::ActionbookCalled {
                action_type,
                outcome,
                error_code,
                ..
            } if action_type == "actionbook_search" => {
                Some((outcome.clone(), error_code.clone()))
            }
            _ => None,
        })
        .expect("ActionbookCalled event present");
    assert_eq!(ab.0, "fail_soft");
    assert_eq!(ab.1.as_deref(), Some("extension_offline"));
}

// ══════════════════════════════════════════════════════════════════════════
// 8. actionbook_action_fail_soft_on_api_key_missing
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn actionbook_action_fail_soft_on_api_key_missing() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig::default());
    let env = TestEnv::new("ab8");
    // Bind mock endpoint but DELIBERATELY unset the API key — this exercises
    // the preflight gate in `executor::preflight_actionbook`.
    let _endpoint_guard = EnvGuard::set("ACTIONBOOK_MCP_ENDPOINT", &mock.endpoint);
    let _backend_guard = EnvGuard::set("ACTIONBOOK_BACKEND", "v2-mcp");
    let _key_guard = EnvGuard::unset("ACTIONBOOK_API_KEY");

    let r1 = response_with_actions(
        "try manual",
        vec![json!({
            "type": "actionbook_manual",
            "site": "x_com",
        })],
        false,
    );
    let r2 = done_response("done");
    let capture = CapturingProvider::new([r1, r2]);
    let prompts = capture.prompts();
    let report = run_loop(&capture, &env);

    assert_eq!(mock.all_cmds().len(), 0, "no MCP RPCs without API key");
    assert_eq!(report.iterations_run, 2, "loop must complete both iters");

    let captured = prompts.lock().unwrap();
    let second_user_prompt = &captured[1];
    assert!(
        second_user_prompt.contains("api key not set"),
        "iter 2 prompt must mention 'api key not set':\n{second_user_prompt}",
    );

    let events = event::read_events(&env.session_jsonl()).unwrap();
    let ab = events
        .iter()
        .find_map(|e| match e {
            SessionEvent::ActionbookCalled {
                outcome,
                error_code,
                ..
            } => Some((outcome.clone(), error_code.clone())),
            _ => None,
        })
        .expect("ActionbookCalled event present");
    assert_eq!(ab.0, "fail_soft");
    assert_eq!(ab.1.as_deref(), Some("api_key_missing"));
}

// ══════════════════════════════════════════════════════════════════════════
// 9. actionbook_action_dry_run_skips_execution
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn actionbook_action_dry_run_skips_execution() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig::default());
    let env = TestEnv::new("ab9");
    let _envguard = mock.bind_env();

    let r1 = response_with_actions(
        "dry-run probe",
        vec![
            json!({
                "type": "actionbook_search",
                "query": "anything",
            }),
            json!({
                "type": "actionbook_run_code",
                "url": "https://x/",
                "script": "f",
            }),
        ],
        false,
    );
    let r2 = done_response("done");
    let provider = FakeProvider::new([r1, r2]);
    let mut cfg = base_cfg();
    cfg.dry_run = true;
    let _report = run_loop_with_cfg(&provider, &env, cfg);

    assert_eq!(
        mock.all_cmds().len(),
        0,
        "dry-run mode must NOT issue any MCP RPC, got: {:?}",
        mock.all_cmds()
    );

    // wiki dir is empty (manual would have seeded if it ran).
    let wiki_entries: Vec<_> = fs::read_dir(env.wiki_dir())
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default();
    assert!(
        wiki_entries.is_empty(),
        "dry-run must not write wiki files: {wiki_entries:?}"
    );

    // session.jsonl event outcome = dry_run.
    let events = event::read_events(&env.session_jsonl()).unwrap();
    let ab_events: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::ActionbookCalled { .. }))
        .collect();
    assert_eq!(ab_events.len(), 2, "expect 2 ActionbookCalled events");
    for e in ab_events {
        if let SessionEvent::ActionbookCalled { outcome, .. } = e {
            assert_eq!(outcome, "dry_run");
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════
// 10. actionbook_unknown_action_field_rejected_in_response_parse  (unit)
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn actionbook_unknown_action_field_rejected_in_response_parse() {
    // Spec: parse must fail with an "unknown field" error message.
    let json = r#"{
        "reasoning":"oops",
        "actions":[{"type":"actionbook_search","query":"x","surprise":"boom"}],
        "done":false
    }"#;
    let err = serde_json::from_str::<LoopResponse>(json)
        .expect_err("parse must reject unknown subfield");
    let msg = err.to_string();
    assert!(
        msg.contains("unknown field") || msg.contains("surprise"),
        "error message must mention unknown field: {msg}",
    );

    // Regression: same protection for the existing 9 variants. Probe `add`.
    let json2 = r#"{
        "reasoning":"oops",
        "actions":[{"type":"add","url":"https://x/","surprise":"boom"}],
        "done":false
    }"#;
    let err2 = serde_json::from_str::<LoopResponse>(json2)
        .expect_err("existing variants must also reject unknown subfields");
    let msg2 = err2.to_string();
    assert!(
        msg2.contains("unknown field") || msg2.contains("surprise"),
        "existing variants must reject unknown subfields: {msg2}",
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 11. actionbook_action_logs_to_session_jsonl
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn actionbook_action_logs_to_session_jsonl() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig {
        search_hits: vec![hit("x_com", None, None)],
        manual_body: "M".to_string(),
        runcode_payload: Some(json!({"text": "r"})),
        ..Default::default()
    });
    let env = TestEnv::new("ab11");
    let _envguard = mock.bind_env();

    let r1 = response_with_actions(
        "all three",
        vec![
            json!({"type":"actionbook_search","query":"q"}),
            json!({"type":"actionbook_manual","site":"x_com","group":"g","action":"a"}),
            json!({"type":"actionbook_run_code","url":"https://x/","script":"f"}),
        ],
        false,
    );
    let r2 = done_response("done");
    let provider = FakeProvider::new([r1, r2]);
    let _report = run_loop(&provider, &env);

    let events = event::read_events(&env.session_jsonl()).unwrap();
    let ab_events: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::ActionbookCalled { .. }))
        .collect();
    assert_eq!(ab_events.len(), 3, "expect 3 ActionbookCalled events");

    let mut saw_search = false;
    let mut saw_manual = false;
    let mut saw_runcode = false;
    for e in ab_events {
        if let SessionEvent::ActionbookCalled {
            iteration,
            action_type,
            cmd_summary,
            outcome,
            wiki_seeded_pages,
            ..
        } = e
        {
            assert_eq!(*iteration, 1);
            assert!(!cmd_summary.is_empty(), "cmd_summary required");
            assert!(!outcome.is_empty(), "outcome required");
            match action_type.as_str() {
                "actionbook_search" => saw_search = true,
                "actionbook_manual" => {
                    saw_manual = true;
                    // wiki_seeded_pages populated for manual when it
                    // wrote a fresh page.
                    assert!(
                        !wiki_seeded_pages.is_empty(),
                        "actionbook_manual should record wiki_seeded_pages",
                    );
                }
                "actionbook_run_code" => saw_runcode = true,
                _ => panic!("unexpected action_type: {action_type}"),
            }
        }
    }
    assert!(saw_search && saw_manual && saw_runcode, "all 3 types must appear");
}

// ══════════════════════════════════════════════════════════════════════════
// 12. actionbook_manual_dedupe_with_existing_wiki_page
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn actionbook_manual_dedupe_with_existing_wiki_page() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig {
        manual_body: "NEW-BODY".to_string(),
        ..Default::default()
    });
    let env = TestEnv::new("ab12");
    let _envguard = mock.bind_env();

    // Pre-seed the wiki page so the dedupe path activates.
    let wiki = env.wiki_dir();
    fs::create_dir_all(&wiki).unwrap();
    let page = wiki.join("x-com-search-search-timeline.md");
    fs::write(&page, "OLD-BODY").unwrap();

    let r1 = response_with_actions(
        "pull manual",
        vec![json!({
            "type": "actionbook_manual",
            "site": "x_com",
            "group": "search",
            "action": "search_timeline",
        })],
        false,
    );
    let r2 = done_response("done");
    let capture = CapturingProvider::new([r1, r2]);
    let prompts = capture.prompts();
    let _report = run_loop(&capture, &env);

    // File contents unchanged on disk.
    let actual = fs::read_to_string(&page).unwrap();
    assert_eq!(actual, "OLD-BODY", "existing file must not be overwritten");

    // Iter 2 prompt still got the NEW-BODY in recent_actionbook_results.
    let captured = prompts.lock().unwrap();
    let second_user_prompt = &captured[1];
    assert!(
        second_user_prompt.contains("NEW-BODY"),
        "LLM context must include latest manual body:\n{second_user_prompt}"
    );

    // session.jsonl event has empty wiki_seeded_pages.
    let events = event::read_events(&env.session_jsonl()).unwrap();
    let ab = events
        .iter()
        .find_map(|e| match e {
            SessionEvent::ActionbookCalled {
                action_type,
                wiki_seeded_pages,
                ..
            } if action_type == "actionbook_manual" => Some(wiki_seeded_pages.clone()),
            _ => None,
        })
        .expect("ActionbookCalled event present");
    assert!(
        ab.is_empty(),
        "wiki_seeded_pages should be empty on dedupe, got: {ab:?}"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 13. actionbook_runcode_timeout_clamped_to_60s  (unit)
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn actionbook_runcode_timeout_clamped_to_60s() {
    use research::autoresearch::executor::{build_user_runcode_cmd, clamp_runcode_timeout};

    // Over-cap → 60s.
    let clamped_max = clamp_runcode_timeout(Some(999_999));
    assert_eq!(clamped_max, 60_000);
    let cmd_max = build_user_runcode_cmd("h", "f", clamped_max);
    assert!(
        cmd_max.contains("--timeout 60000"),
        "expected --timeout 60000 in: {cmd_max}"
    );

    // Default (None) → 30s.
    let clamped_default = clamp_runcode_timeout(None);
    assert_eq!(clamped_default, 30_000);
    let cmd_default = build_user_runcode_cmd("h", "f", clamped_default);
    assert!(
        cmd_default.contains("--timeout 30000"),
        "expected --timeout 30000 in: {cmd_default}"
    );

    // Under-min → 5s.
    let clamped_min = clamp_runcode_timeout(Some(100));
    assert_eq!(clamped_min, 5_000);
    let cmd_min = build_user_runcode_cmd("h", "f", clamped_min);
    assert!(
        cmd_min.contains("--timeout 5000"),
        "expected --timeout 5000 in: {cmd_min}"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// 14. actionbook_action_v1_backend_skips
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn actionbook_action_v1_backend_skips() {
    let _lock = env_serializer();
    let mock = McpMock::start_with(MockConfig::default());
    let env = TestEnv::new("ab14");
    let _envguard = mock.bind_env();
    let _backend = EnvGuard::set("ACTIONBOOK_BACKEND", "v1-cli");

    let r1 = response_with_actions(
        "try search under v1",
        vec![json!({
            "type": "actionbook_search",
            "query": "x",
        })],
        false,
    );
    let r2 = done_response("done");
    let capture = CapturingProvider::new([r1, r2]);
    let prompts = capture.prompts();
    let _report = run_loop(&capture, &env);

    assert_eq!(mock.all_cmds().len(), 0, "no MCP RPCs under v1 backend");

    let captured = prompts.lock().unwrap();
    let second_user_prompt = &captured[1];
    assert!(
        second_user_prompt.contains("backend is v1 cli"),
        "iter 2 prompt must mention 'backend is v1 cli':\n{second_user_prompt}",
    );

    let events = event::read_events(&env.session_jsonl()).unwrap();
    let code = events
        .iter()
        .find_map(|e| match e {
            SessionEvent::ActionbookCalled { error_code, .. } => error_code.clone(),
            _ => None,
        })
        .expect("ActionbookCalled event present");
    assert_eq!(code, "v1_backend_no_mcp");
}

// ══════════════════════════════════════════════════════════════════════════
// Helpers
// ══════════════════════════════════════════════════════════════════════════

fn hit(site: &str, group: Option<&str>, action: Option<&str>) -> Value {
    let mut o = serde_json::Map::new();
    o.insert("site".into(), Value::String(site.into()));
    if let Some(g) = group {
        o.insert("group".into(), Value::String(g.into()));
    }
    if let Some(a) = action {
        o.insert("action".into(), Value::String(a.into()));
    }
    Value::Object(o)
}

fn response_with_actions(reasoning: &str, actions: Vec<Value>, done: bool) -> String {
    json!({
        "reasoning": reasoning,
        "actions": actions,
        "done": done,
    })
    .to_string()
}

fn done_response(reason: &str) -> String {
    json!({
        "reasoning": "wrapping up",
        "actions": [],
        "done": true,
        "reason": reason,
    })
    .to_string()
}

fn base_cfg() -> LoopConfig {
    LoopConfig {
        iterations: 2,
        max_actions: 20,
        dry_run: false,
    }
}

fn run_loop(provider: &dyn AgentProvider, env: &TestEnv) -> executor::LoopReport {
    run_loop_with_cfg(provider, env, base_cfg())
}

fn run_loop_with_cfg(
    provider: &dyn AgentProvider,
    env: &TestEnv,
    cfg: LoopConfig,
) -> executor::LoopReport {
    let bin = research_bin();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(executor::run(provider, &env.slug, cfg, &bin))
}

/// Pull the injected `text` field out of the user prompt's
/// `recent_actionbook_results` JSON block. The block is emitted by
/// `serde_json::to_string_pretty` and contains `"text": "..."` —
/// unescape minimally to undo `\\n` / `\\u00b7` style escapes.
fn extract_injected_text(prompt: &str) -> Option<String> {
    let key = "\"text\": \"";
    let start = prompt.find(key)? + key.len();
    let mut out = String::new();
    let mut chars = prompt[start..].chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'u' => {
                    let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                    if let Ok(n) = u32::from_str_radix(&hex, 16)
                        && let Some(ch) = char::from_u32(n)
                    {
                        out.push(ch);
                    }
                }
                other => out.push(other),
            }
        } else if c == '"' {
            return Some(out);
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// Per-test environment: tempdir + scoped `ACTIONBOOK_RESEARCH_HOME` env.
/// Bootstraps the session directory tree (mkdir `<home>/<slug>` + write
/// a minimal `session.toml` + `session.md` containing a `## Plan` so the
/// first-iteration plan guard doesn't reject the actionbook actions).
struct TestEnv {
    _tmp: TempDir,
    home: String,
    slug: String,
    _home_guard: EnvGuard,
}

impl TestEnv {
    fn new(slug: &str) -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let slug = slug.to_string();
        let session_dir = tmp.path().join(&slug);
        fs::create_dir_all(&session_dir).unwrap();
        // session.toml — minimal for `config::exists` + `config::read`.
        fs::write(
            session_dir.join("session.toml"),
            format!(
                "slug = \"{slug}\"\ntopic = \"test\"\npreset = \"tech\"\ntags = []\ncreated_at = \"2026-05-17T00:00:00Z\"\n",
            ),
        )
        .unwrap();
        // session.md — already has a `## Plan` so the first-iter plan
        // guard accepts arbitrary actions.
        fs::write(
            session_dir.join("session.md"),
            "# test\n\n## Overview\nsomething.\n\n## Plan\nplaceholder.\n",
        )
        .unwrap();
        // session.jsonl — empty file so read_events doesn't error on path.
        fs::write(session_dir.join("session.jsonl"), "").unwrap();
        let _home_guard = EnvGuard::set("ACTIONBOOK_RESEARCH_HOME", &home);
        Self {
            _tmp: tmp,
            home,
            slug,
            _home_guard,
        }
    }

    fn session_dir(&self) -> PathBuf {
        PathBuf::from(&self.home).join(&self.slug)
    }

    fn session_jsonl(&self) -> PathBuf {
        self.session_dir().join("session.jsonl")
    }

    fn wiki_dir(&self) -> PathBuf {
        self.session_dir().join("wiki")
    }
}

/// Scoped env-var setter that restores the prior value on drop.
struct EnvGuard {
    key: String,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &str, val: &str) -> Self {
        let prev = std::env::var(key).ok();
        unsafe { std::env::set_var(key, val) };
        Self {
            key: key.to_string(),
            prev,
        }
    }

    fn unset(key: &str) -> Self {
        let prev = std::env::var(key).ok();
        unsafe { std::env::remove_var(key) };
        Self {
            key: key.to_string(),
            prev,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => unsafe { std::env::set_var(&self.key, v) },
            None => unsafe { std::env::remove_var(&self.key) },
        }
    }
}

/// FakeProvider variant that records every user prompt it sees so tests
/// can assert on what landed in `recent_actionbook_results` between
/// iterations.
struct CapturingProvider {
    inner: FakeProvider,
    prompts: Arc<Mutex<Vec<String>>>,
}

impl CapturingProvider {
    fn new<I, S>(responses: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            inner: FakeProvider::new(responses),
            prompts: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn prompts(&self) -> Arc<Mutex<Vec<String>>> {
        self.prompts.clone()
    }
}

#[async_trait]
impl AgentProvider for CapturingProvider {
    async fn ask(&self, system: &str, user: &str) -> Result<String, ProviderError> {
        self.prompts.lock().unwrap().push(user.to_string());
        self.inner.ask(system, user).await
    }

    fn name(&self) -> &'static str {
        "fake"
    }
}

// ══════════════════════════════════════════════════════════════════════════
// Mock MCP server (HTTP)
// ══════════════════════════════════════════════════════════════════════════

#[derive(Default, Clone)]
struct MockConfig {
    /// Hits returned for `actionbook search` cmds. Empty = "no hit".
    search_hits: Vec<Value>,
    /// If Some, the mock returns this error code in the JSON-RPC error
    /// envelope for `search` cmds.
    search_error_code: Option<String>,
    /// Body returned for successful `actionbook manual` cmds.
    manual_body: String,
    /// JSON payload returned by `browser run-code` cmds. The mock wraps
    /// it as `[t1]\nok browser run-code\n<json>` to match the V2 envelope
    /// shape `extract_run_code_payload` parses.
    runcode_payload: Option<Value>,
}

struct McpMock {
    endpoint: String,
    cmds: Arc<Mutex<Vec<String>>>,
}

impl McpMock {
    fn start_with(config: MockConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().unwrap();
        let cmds: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let cmds_for_thread = cmds.clone();
        let cfg = config;
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let cmds = cmds_for_thread.clone();
                let cfg = cfg.clone();
                thread::spawn(move || {
                    let mut buf = [0u8; 65536];
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
                        let mut more = [0u8; 65536];
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
                            let cmd = parsed
                                .get("params")
                                .and_then(|p| p.get("arguments"))
                                .and_then(|a| a.get("cmd"))
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            cmds.lock().unwrap().push(cmd.clone());

                            extra_headers = "";
                            if cmd.starts_with("actionbook search") {
                                if let Some(code) = &cfg.search_error_code {
                                    response_json = json!({
                                        "jsonrpc": "2.0",
                                        "id": parsed.get("id").cloned().unwrap_or(Value::Null),
                                        "error": { "code": code, "message": code }
                                    })
                                    .to_string();
                                } else {
                                    let text = serde_json::to_string(&cfg.search_hits)
                                        .unwrap_or_else(|_| "[]".to_string());
                                    response_json = json!({
                                        "jsonrpc": "2.0",
                                        "id": parsed.get("id").cloned().unwrap_or(Value::Null),
                                        "result": {
                                            "content": [{"type":"text","text": text}]
                                        }
                                    })
                                    .to_string();
                                }
                            } else if cmd.starts_with("actionbook manual") {
                                let text = format!("[t1]\nok actionbook manual\n{}", cfg.manual_body);
                                response_json = json!({
                                    "jsonrpc": "2.0",
                                    "id": parsed.get("id").cloned().unwrap_or(Value::Null),
                                    "result": {
                                        "content": [{"type":"text","text": text}]
                                    }
                                })
                                .to_string();
                            } else if cmd.starts_with("browser run-code") {
                                let payload = cfg
                                    .runcode_payload
                                    .clone()
                                    .unwrap_or_else(|| json!({"text": "ok"}));
                                let text = format!(
                                    "[t1]\nok browser run-code\n{}",
                                    serde_json::to_string(&payload).unwrap()
                                );
                                response_json = json!({
                                    "jsonrpc": "2.0",
                                    "id": parsed.get("id").cloned().unwrap_or(Value::Null),
                                    "result": {
                                        "content": [{"type":"text","text": text}]
                                    }
                                })
                                .to_string();
                            } else {
                                // new-tab / close / unknown — return success envelope.
                                response_json = json!({
                                    "jsonrpc": "2.0",
                                    "id": parsed.get("id").cloned().unwrap_or(Value::Null),
                                    "result": {
                                        "content": [{"type":"text","text":""}]
                                    }
                                })
                                .to_string();
                            }
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

    fn bind_env(&self) -> Vec<EnvGuard> {
        vec![
            EnvGuard::set("ACTIONBOOK_MCP_ENDPOINT", &self.endpoint),
            EnvGuard::set("ACTIONBOOK_API_KEY", "test-key"),
            EnvGuard::set("ACTIONBOOK_BACKEND", "v2-mcp"),
        ]
    }

    fn all_cmds(&self) -> Vec<String> {
        self.cmds.lock().unwrap().clone()
    }

    fn search_cmds(&self) -> Vec<String> {
        self.cmds
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("actionbook search"))
            .cloned()
            .collect()
    }

    fn runcode_cmds(&self) -> Vec<String> {
        self.cmds
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("browser run-code"))
            .cloned()
            .collect()
    }
}

// Silence unused import warnings in feature-gate trimmed builds.
#[allow(dead_code)]
fn _unused_action_marker(_a: Action) {}
