//! Integration tests for the autonomous research loop.
//!
//! Feature-gated: only compiles under `--features autoresearch`. Uses the
//! `FakeProvider` end-to-end so no real LLM is touched.

#![cfg(feature = "autoresearch")]

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

fn research_bin() -> String {
    env!("CARGO_BIN_EXE_ascent-research").to_string()
}

struct Env {
    _tmp: TempDir,
    home: String,
}

impl Env {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        Self { _tmp: tmp, home }
    }

    fn research(&self, args: &[&str]) -> (Value, i32, String) {
        let mut cmd = Command::new(research_bin());
        cmd.args(args);
        cmd.env("ACTIONBOOK_RESEARCH_HOME", &self.home);
        let out = cmd.output().expect("spawn research");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let v: Value = stdout
            .lines()
            .find(|l| l.trim_start().starts_with('{'))
            .and_then(|l| serde_json::from_str(l).ok())
            .unwrap_or(Value::Null);
        (v, out.status.code().unwrap_or(-1), stderr)
    }

    fn session_dir(&self, slug: &str) -> PathBuf {
        PathBuf::from(&self.home).join(slug)
    }

    fn prep(&self, slug: &str, body: &str) {
        let (_, code, stderr) = self.research(&["new", "topic", "--slug", slug, "--json"]);
        assert_eq!(code, 0, "new failed: {stderr}");
        // v2 enforces `## Plan` on iter 1. Tests focused on other behavior
        // get a placeholder plan so the guard doesn't trip them. Tests that
        // actually exercise plan enforcement call `prep_no_plan` instead.
        let body_out = if body.contains("## Plan") {
            body.to_string()
        } else {
            format!("{body}\n## Plan\nplaceholder plan for tests.\n")
        };
        fs::write(self.session_dir(slug).join("session.md"), body_out).unwrap();
    }

    fn prep_no_plan(&self, slug: &str, body: &str) {
        let (_, code, stderr) = self.research(&["new", "topic", "--slug", slug, "--json"]);
        assert_eq!(code, 0, "new failed: {stderr}");
        fs::write(self.session_dir(slug).join("session.md"), body).unwrap();
    }

    /// Fake provider takes responses joined by ASCII Record Separator (0x1e).
    fn loop_cmd(&self, slug: &str, responses: &[&str], extra: &[&str]) -> (Value, i32, String) {
        let joined = responses.join("\u{1e}");
        let mut args: Vec<&str> = vec![
            "loop",
            slug,
            "--provider",
            "fake",
            "--fake-responses",
            &joined,
            "--json",
        ];
        args.extend_from_slice(extra);
        self.research(&args)
    }
}

fn r_done(reason: &str) -> String {
    format!(
        r#"{{"reasoning":"wrapping up","actions":[],"done":true,"reason":"{reason}"}}"#
    )
}

fn r_write_overview(body: &str) -> String {
    format!(
        r#"{{"reasoning":"draft overview","actions":[{{"type":"write_overview","body":"{body}"}}],"done":false}}"#
    )
}

fn r_empty_noop() -> String {
    r#"{"reasoning":"think","actions":[],"done":false}"#.to_string()
}

// ── Test 1: happy path single iteration, done ─────────────────────────────

#[test]
fn loop_runs_one_round_and_terminates_on_done() {
    let env = Env::new();
    env.prep("l1", "## Overview\nsomething real.\n");

    let done = r_done("all set");
    let responses = [done.as_str()];
    let (v, code, stderr) = env.loop_cmd("l1", &responses, &[]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(v["data"]["provider"], "fake");
    assert_eq!(v["data"]["iterations_run"], 1);
    assert_eq!(v["data"]["termination_reason"], "provider_done");
    assert_eq!(v["data"]["actions_executed"], 0);
}

// ── Test 2: schema violation skips iteration but continues ────────────────

#[test]
fn loop_skips_iteration_on_schema_violation() {
    let env = Env::new();
    env.prep("l2", "## Overview\nsomething real.\n");

    let done = r_done("recovered");
    let responses = [r#"not json at all"#, done.as_str()];
    let (v, code, _) = env.loop_cmd("l2", &responses, &[]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["iterations_run"], 2);
    let warnings: Vec<String> = v["data"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap().to_string())
        .collect();
    assert!(warnings.iter().any(|w| w.contains("schema_violation")));
}

// ── Test 3: iterations exhausted without done ─────────────────────────────

#[test]
fn loop_terminates_on_iterations_exhausted() {
    let env = Env::new();
    env.prep("l3", "## Overview\nsomething real.\n");

    // Three noops, but we cap at 2 iterations.
    let responses = [r_empty_noop(), r_empty_noop(), r_empty_noop()];
    let refs: Vec<&str> = responses.iter().map(|s| s.as_str()).collect();
    let (v, code, _) = env.loop_cmd("l3", &refs, &["--iterations", "2"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["iterations_run"], 2);
    assert_eq!(v["data"]["termination_reason"], "iterations_exhausted");
}

// ── Test 4: max-actions cap stops mid-iteration ───────────────────────────

#[test]
fn loop_respects_max_actions_cap() {
    let env = Env::new();
    env.prep("l4", "## Overview\nsomething real.\n");

    // One iteration proposes 3 add actions; --max-actions 2 should stop
    // after the 2nd.
    let big = String::from(
        r#"{"reasoning":"bulk","actions":[{"type":"add","url":"https://a.test/"},{"type":"add","url":"https://b.test/"},{"type":"add","url":"https://c.test/"}],"done":false}"#,
    );
    let done = r_done("stopped");
    let (v, code, _) = env.loop_cmd(
        "l4",
        &[big.as_str(), done.as_str()],
        &["--max-actions", "2", "--dry-run"],
    );
    assert_eq!(code, 0);
    // With --dry-run actions don't actually execute subprocess, but the cap
    // still counts successful dispatches — so 2 are counted executed, then
    // max_actions_exhausted trips before the third.
    assert_eq!(v["data"]["actions_executed"], 2);
    assert_eq!(v["data"]["termination_reason"], "max_actions_exhausted");
}

// ── Test 5: dry-run does not touch session files ──────────────────────────

#[test]
fn loop_dry_run_does_not_modify_session() {
    let env = Env::new();
    env.prep("l5", "## Overview\noriginal overview text.\n");
    let md_before = fs::read_to_string(env.session_dir("l5").join("session.md")).unwrap();

    let write = r_write_overview("BRAND NEW OVERVIEW");
    let done = r_done("done");
    let (v, code, _) = env.loop_cmd(
        "l5",
        &[write.as_str(), done.as_str()],
        &["--dry-run"],
    );
    assert_eq!(code, 0);
    assert_eq!(v["data"]["actions_executed"], 1);

    let md_after = fs::read_to_string(env.session_dir("l5").join("session.md")).unwrap();
    assert_eq!(md_before, md_after, "dry-run should not modify session.md");
    // BUT the loop events should still have been logged to jsonl.
    let jsonl = fs::read_to_string(env.session_dir("l5").join("session.jsonl")).unwrap();
    assert!(jsonl.contains(r#""event":"loop_started""#));
    assert!(jsonl.contains(r#""event":"loop_step""#));
    assert!(jsonl.contains(r#""event":"loop_completed""#));
}

// ── Test 6: write_overview actually replaces the section ──────────────────

#[test]
fn loop_write_overview_replaces_body() {
    let env = Env::new();
    env.prep("l6", "## Overview\nold content.\n\n## 01 · WHY\nwhy body.\n");

    let write = r_write_overview("fresh overview by the loop");
    let done = r_done("done");
    let (v, code, stderr) = env.loop_cmd("l6", &[write.as_str(), done.as_str()], &[]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(v["data"]["actions_executed"], 1);

    let md = fs::read_to_string(env.session_dir("l6").join("session.md")).unwrap();
    assert!(md.contains("fresh overview by the loop"));
    assert!(!md.contains("old content"));
    // Next section must survive.
    assert!(md.contains("## 01 · WHY"));
    assert!(md.contains("why body"));
}

// ── Test 7: write_section inserts when heading missing ────────────────────

#[test]
fn loop_write_section_inserts_missing_heading() {
    let env = Env::new();
    env.prep("l7", "## Overview\nbase.\n");

    let write = r###"{"reasoning":"create","actions":[{"type":"write_section","heading":"## 01 · WHY","body":"inserted body"}],"done":false}"###;
    let done = r_done("done");
    let (v, code, _) = env.loop_cmd("l7", &[write, done.as_str()], &[]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["actions_executed"], 1);

    let md = fs::read_to_string(env.session_dir("l7").join("session.md")).unwrap();
    assert!(md.contains("## 01 · WHY"));
    assert!(md.contains("inserted body"));
}

// ── Test 8: diagram-needed records a TODO comment ─────────────────────────

#[test]
fn loop_diagram_needed_appends_todo_comment() {
    let env = Env::new();
    env.prep("l8", "## Overview\nbase.\n");

    let note = r#"{"reasoning":"need viz","actions":[{"type":"note_diagram_needed","name":"axis.svg","hint":"x=business,y=hype"}],"done":false}"#;
    let done = r_done("done");
    let (v, code, _) = env.loop_cmd("l8", &[note, done.as_str()], &[]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["actions_executed"], 1);

    let md = fs::read_to_string(env.session_dir("l8").join("session.md")).unwrap();
    assert!(md.contains("research-loop: diagram needed"));
    assert!(md.contains("axis.svg"));
    assert!(md.contains("x=business,y=hype"));
}

// ── Test 9: jsonl carries loop events ─────────────────────────────────────

#[test]
fn loop_writes_start_step_and_completed_events() {
    let env = Env::new();
    env.prep("l9", "## Overview\nbase.\n");

    let done = r_done("immediate");
    let (v, code, _) = env.loop_cmd("l9", &[done.as_str()], &["--iterations", "1"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["iterations_run"], 1);

    let jsonl = fs::read_to_string(env.session_dir("l9").join("session.jsonl")).unwrap();
    assert_eq!(
        jsonl.lines().filter(|l| l.contains(r#""event":"loop_started""#)).count(),
        1
    );
    assert_eq!(
        jsonl.lines().filter(|l| l.contains(r#""event":"loop_step""#)).count(),
        1
    );
    assert_eq!(
        jsonl.lines().filter(|l| l.contains(r#""event":"loop_completed""#)).count(),
        1
    );
}

// ── Test 10: unknown provider rejected with clear code ────────────────────

#[test]
fn loop_unknown_provider_returns_provider_not_available() {
    let env = Env::new();
    env.prep("l10", "## Overview\nbase.\n");

    let (v, code, _) = env.research(&[
        "loop", "l10", "--provider", "mystery", "--json",
    ]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "PROVIDER_NOT_AVAILABLE");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("mystery")
    );
}

// ── Test 11: session not found ────────────────────────────────────────────

#[test]
fn loop_session_not_found() {
    let env = Env::new();
    let (v, code, _) = env.research(&[
        "loop", "nope", "--provider", "fake", "--json",
    ]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "SESSION_NOT_FOUND");
}

// v2 Step 1 — Per-source digestion ────────────────────────────────────────

/// Pre-seed a `source_accepted` jsonl line so `digest_source` has something
/// legal to target. Returns nothing; test uses `url` to verify side effects.
fn seed_accepted(env: &Env, slug: &str, url: &str) {
    use std::io::Write;
    let line = format!(
        r#"{{"event":"source_accepted","timestamp":"2026-04-19T12:00:00Z","url":"{url}","kind":"arxiv-abs","executor":"postagent","raw_path":"raw/1-arxiv.json","bytes":1000,"trust_score":2.0}}"#
    );
    let path = env.session_dir(slug).join("session.jsonl");
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open session.jsonl for append");
    writeln!(f, "{line}").unwrap();
}

// ── Test 12: digest_source writes source_digested event ──────────────────

#[test]
fn loop_digest_source_writes_jsonl_event() {
    let env = Env::new();
    env.prep("d1", "## Overview\nbase.\n");
    let url = "https://arxiv.org/abs/2401.12345";
    seed_accepted(&env, "d1", url);

    let digest = format!(
        r###"{{"reasoning":"digest paper","actions":[{{"type":"digest_source","url":"{url}","into_section":"## 02 · WHAT"}}],"done":false}}"###
    );
    let done = r_done("done");
    let (v, code, stderr) = env.loop_cmd("d1", &[digest.as_str(), done.as_str()], &[]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(v["data"]["actions_executed"], 1);

    let jsonl = fs::read_to_string(env.session_dir("d1").join("session.jsonl")).unwrap();
    assert!(
        jsonl.contains(r#""event":"source_digested""#),
        "expected source_digested event in jsonl; got:\n{jsonl}"
    );
    assert!(jsonl.contains(url), "digest event should carry the URL");
    assert!(
        jsonl.contains(r###""into_section":"## 02 · WHAT""###),
        "into_section should be preserved"
    );
}

// v2 Step 4 — first-iter plan enforcement ────────────────────────────────

// ── Test 13a: iter 1 without a plan rejects non-plan actions ────────────

#[test]
fn loop_first_iter_enforces_plan_when_absent() {
    let env = Env::new();
    env.prep_no_plan("p1", "## Overview\nbase content.\n");

    // Fake returns a write_overview on iter 1 (not a plan). Expect reject.
    let write = r_write_overview("oops forgot the plan");
    let done = r_done("stop");
    let (v, code, stderr) = env.loop_cmd("p1", &[write.as_str(), done.as_str()], &[]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(
        v["data"]["actions_executed"], 0,
        "iter 1 non-plan action must be rejected"
    );
    let warnings: Vec<String> = v["data"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap().to_string())
        .collect();
    assert!(
        warnings.iter().any(|w| w.contains("plan_required")),
        "expected plan_required warning; got: {warnings:?}"
    );
}

// ── Test 13b: write_plan inserts `## Plan` and emits plan_written ───────

#[test]
fn loop_write_plan_inserts_section_and_writes_event() {
    let env = Env::new();
    env.prep_no_plan("p2", "## Overview\nbase.\n\n## 01 · A\nbody.\n");

    let plan = r###"{"reasoning":"plan first","actions":[{"type":"write_plan","body":"step 1 fetch arxiv\nstep 2 fetch github"}],"done":false}"###;
    let done = r_done("stop");
    let (v, code, stderr) = env.loop_cmd("p2", &[plan, done.as_str()], &[]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(v["data"]["actions_executed"], 1);

    let md = fs::read_to_string(env.session_dir("p2").join("session.md")).unwrap();
    assert!(md.contains("## Plan"));
    assert!(md.contains("step 1 fetch arxiv"));
    // Must sit between Overview and the first numbered section.
    let overview_idx = md.find("## Overview").unwrap();
    let plan_idx = md.find("## Plan").unwrap();
    let section_idx = md.find("## 01").unwrap();
    assert!(
        overview_idx < plan_idx && plan_idx < section_idx,
        "plan must land between Overview and ## 01; md:\n{md}"
    );

    let jsonl = fs::read_to_string(env.session_dir("p2").join("session.jsonl")).unwrap();
    assert!(
        jsonl.contains(r#""event":"plan_written""#),
        "expected plan_written event in jsonl"
    );
}

// ── Test 13c: plan-then-other-action in same iter is allowed ────────────

#[test]
fn loop_plan_satisfied_allows_other_actions_same_iter() {
    let env = Env::new();
    env.prep_no_plan("p3", "## Overview\nbase.\n\n## 01 · A\nbody.\n");

    let iter1 = r###"{"reasoning":"plan then write","actions":[{"type":"write_plan","body":"plan body"},{"type":"write_section","heading":"## 02 · WHY","body":"why body"}],"done":false}"###;
    let done = r_done("stop");
    let (v, code, _) = env.loop_cmd("p3", &[iter1, done.as_str()], &[]);
    assert_eq!(code, 0);
    assert_eq!(
        v["data"]["actions_executed"], 2,
        "once plan lands mid-iter, subsequent actions this turn are allowed"
    );
}

// v2 Step 3 — write_diagram action ────────────────────────────────────────

// ── Test 13d: valid SVG lands on disk + emits diagram_authored ──────────

#[test]
fn loop_write_diagram_saves_svg_file() {
    let env = Env::new();
    env.prep("g1", "## Overview\nbase.\n");

    // Raw-string-safe JSON: backslash-quotes inside the svg attribute are
    // literal `\"` bytes on the wire, which JSON decodes back to `"`.
    let payload = r###"{"reasoning":"draw","actions":[{"type":"write_diagram","path":"quad.svg","alt":"quadrant","svg":"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 100 100\"><rect width=\"100\" height=\"100\"/></svg>"}],"done":false}"###;
    let done = r_done("done");
    let (v, code, stderr) = env.loop_cmd("g1", &[payload, done.as_str()], &[]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(v["data"]["actions_executed"], 1);

    let svg_path = env.session_dir("g1").join("diagrams").join("quad.svg");
    assert!(svg_path.is_file(), "validated SVG must be written to disk");
    let body = fs::read_to_string(&svg_path).unwrap();
    assert!(body.contains("<svg"));

    let jsonl = fs::read_to_string(env.session_dir("g1").join("session.jsonl")).unwrap();
    assert!(
        jsonl.contains(r#""event":"diagram_authored""#),
        "expected diagram_authored event"
    );
}

// ── Test 13e: SVG containing <script> is rejected + diagram_rejected ────

#[test]
fn loop_write_diagram_rejects_script_tag() {
    let env = Env::new();
    env.prep("g2", "## Overview\nbase.\n");

    let payload = r###"{"reasoning":"bad svg","actions":[{"type":"write_diagram","path":"bad.svg","alt":"x","svg":"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>"}],"done":false}"###;
    let done = r_done("done");
    let (v, code, _) = env.loop_cmd("g2", &[payload, done.as_str()], &[]);
    assert_eq!(code, 0);
    assert_eq!(
        v["data"]["actions_executed"], 0,
        "hostile SVG must be rejected before any write"
    );

    let svg_path = env.session_dir("g2").join("diagrams").join("bad.svg");
    assert!(!svg_path.exists(), "rejected SVG must not touch disk");

    let jsonl = fs::read_to_string(env.session_dir("g2").join("session.jsonl")).unwrap();
    assert!(
        jsonl.contains(r#""event":"diagram_rejected""#),
        "expected diagram_rejected event; got:\n{jsonl}"
    );
    let warnings: Vec<String> = v["data"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap().to_string())
        .collect();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("svg_schema_violation") || w.contains("script")),
        "expected svg_schema_violation warning; got: {warnings:?}"
    );
}

// ── Test 14: re-digesting the same URL is rejected (proves filter wiring) ─

#[test]
fn loop_subsequent_iter_sees_digested_sources_excluded() {
    let env = Env::new();
    env.prep("d2", "## Overview\nbase.\n");
    let url = "https://arxiv.org/abs/2401.99999";
    seed_accepted(&env, "d2", url);

    let digest = format!(
        r###"{{"reasoning":"first digest","actions":[{{"type":"digest_source","url":"{url}","into_section":"## 01"}}],"done":false}}"###
    );
    let digest_again = format!(
        r###"{{"reasoning":"retry","actions":[{{"type":"digest_source","url":"{url}","into_section":"## 01"}}],"done":false}}"###
    );
    let done = r_done("done");
    let (v, code, stderr) = env.loop_cmd(
        "d2",
        &[digest.as_str(), digest_again.as_str(), done.as_str()],
        &["--iterations", "3"],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    // First digest succeeds, second rejected — total executed = 1.
    assert_eq!(
        v["data"]["actions_executed"], 1,
        "second digest_source on same URL must be rejected"
    );
    let warnings: Vec<String> = v["data"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap().to_string())
        .collect();
    assert!(
        warnings.iter().any(|w| w.contains("source_already_digested")),
        "expected source_already_digested warning; got: {warnings:?}"
    );
}
