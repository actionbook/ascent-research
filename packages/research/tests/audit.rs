use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn research_bin() -> String {
    env!("CARGO_BIN_EXE_ascent-research").to_string()
}

fn research_with_home(home: &Path, args: &[&str]) -> (Value, i32, String, String) {
    let out = Command::new(research_bin())
        .args(args)
        .env("HOME", home)
        .env_remove("ACTIONBOOK_RESEARCH_HOME")
        .output()
        .expect("spawn ascent-research");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let json_line = stdout.lines().find(|l| l.trim_start().starts_with('{'));
    let v: Value = match json_line {
        Some(l) => serde_json::from_str(l).unwrap_or(Value::Null),
        None => Value::Null,
    };
    (v, out.status.code().unwrap_or(-1), stderr, stdout)
}

fn write_legacy_audit_session(home: &Path, slug: &str) {
    let dir = home.join(".actionbook").join("research").join(slug);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("session.toml"),
        format!(
            r#"slug = "{slug}"
topic = "legacy audit topic"
preset = "tech"
created_at = "2026-04-20T10:00:00Z"
"#
        ),
    )
    .unwrap();
    fs::write(dir.join("session.md"), "# Research\n").unwrap();
    fs::write(
        dir.join("report.html"),
        r#"<div class="lang-switch"><button data-mode="zh">中文</button></div><p class="tr-zh">中文。</p>"#,
    )
    .unwrap();
    fs::write(
        dir.join("session.jsonl"),
        format!(
            r#"{{"event":"synthesize_started","timestamp":"2026-04-20T10:00:03Z","bilingual":true,"bilingual_provider":"codex"}}
{{"event":"synthesize_completed","timestamp":"2026-04-20T10:00:04Z","report_json_path":"report.json","report_html_path":"{slug}/report.html","accepted_sources":0,"rejected_sources":0,"duration_ms":42}}
"#
        ),
    )
    .unwrap();
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

    fn research(&self, args: &[&str]) -> (Value, i32, String, String) {
        let out = Command::new(research_bin())
            .args(args)
            .env("ACTIONBOOK_RESEARCH_HOME", &self.home)
            .output()
            .expect("spawn ascent-research");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let json_line = stdout.lines().find(|l| l.trim_start().starts_with('{'));
        let v: Value = match json_line {
            Some(l) => serde_json::from_str(l).unwrap_or(Value::Null),
            None => Value::Null,
        };
        (v, out.status.code().unwrap_or(-1), stderr, stdout)
    }

    fn new_session(&self, slug: &str, tags: &[&str]) {
        let mut args = vec!["--json", "new", "audit topic", "--slug", slug];
        for tag in tags {
            args.push("--tag");
            args.push(tag);
        }
        let (v, code, stderr, _) = self.research(&args);
        assert_eq!(code, 0, "new failed: stderr={stderr}; envelope={v}");
    }

    fn session_dir(&self, slug: &str) -> PathBuf {
        PathBuf::from(&self.home).join(slug)
    }

    fn jsonl_path(&self, slug: &str) -> PathBuf {
        self.session_dir(slug).join("session.jsonl")
    }

    fn append_jsonl(&self, slug: &str, lines: &[String]) {
        let path = self.jsonl_path(slug);
        let mut current = fs::read_to_string(&path).unwrap_or_default();
        for line in lines {
            current.push_str(line);
            current.push('\n');
        }
        fs::write(path, current).unwrap();
    }
}

fn source_accepted(url: &str) -> String {
    format!(
        r#"{{"event":"source_accepted","timestamp":"2026-04-20T10:00:00Z","url":"{url}","kind":"nba-team-roster","executor":"browser","raw_path":"raw/1-nba-team-roster.json","bytes":2048,"trust_score":2.0}}"#
    )
}

fn source_digested(url: &str) -> String {
    format!(
        r###"{{"event":"source_digested","timestamp":"2026-04-20T10:00:30Z","iteration":1,"url":"{url}","into_section":"## Overview"}}"###
    )
}

fn tool_started(call_id: &str) -> String {
    format!(
        r#"{{"event":"tool_call_started","timestamp":"2026-04-20T10:00:01Z","call_id":"{call_id}","hand":"browser","tool":"actionbook browser","input_summary":"url=https://www.nba.com/lakers/roster"}}"#
    )
}

fn tool_completed(call_id: &str, status: &str) -> String {
    format!(
        r#"{{"event":"tool_call_completed","timestamp":"2026-04-20T10:00:02Z","call_id":"{call_id}","status":"{status}","duration_ms":321,"output_summary":"bytes=2048 warnings=0","artifact_refs":["raw/1-nba-team-roster.json"]}}"#
    )
}

fn fact_checked(source: &str, outcome: &str) -> String {
    format!(
        r###"{{"event":"fact_checked","timestamp":"2026-04-20T10:00:03Z","iteration":1,"claim":"Current roster claim","query":"official roster current","sources":["{source}"],"outcome":"{outcome}","into_section":"## Overview","note":"official roster"}}"###
    )
}

fn synthesize_completed() -> String {
    r#"{"event":"synthesize_completed","timestamp":"2026-04-20T10:00:04Z","report_json_path":"report.json","report_html_path":"report.html","accepted_sources":1,"rejected_sources":0,"duration_ms":42}"#.to_string()
}

fn synthesize_started_bilingual(provider: &str) -> String {
    format!(
        r#"{{"event":"synthesize_started","timestamp":"2026-04-20T10:00:03Z","bilingual":true,"bilingual_provider":"{provider}"}}"#
    )
}

fn array_contains_str(arr: &[Value], needle: &str) -> bool {
    arr.iter()
        .filter_map(Value::as_str)
        .any(|s| s.contains(needle))
}

fn contains_file_named(root: &Path, filename: &str) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().and_then(|name| name.to_str()) == Some(filename) {
            return true;
        }
        if path.is_dir() && contains_file_named(&path, filename) {
            return true;
        }
    }
    false
}

#[test]
fn audit_summarizes_tool_and_fact_check_trace() {
    let env = Env::new();
    let slug = "audit-complete";
    let url = "https://www.nba.com/lakers/roster";
    env.new_session(slug, &["fact-check"]);
    env.append_jsonl(
        slug,
        &[
            tool_started("call-1"),
            tool_completed("call-1", "ok"),
            source_accepted(url),
            source_digested(url),
            fact_checked(url, "supported"),
            synthesize_completed(),
        ],
    );

    let (v, code, stderr, _) = env.research(&["--json", "audit", slug]);
    assert_eq!(code, 0, "stderr={stderr}; envelope={v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["command"], "research audit");
    assert_eq!(v["data"]["audit_status"], "complete");
    assert_eq!(v["data"]["tools"]["started"], 1);
    assert_eq!(v["data"]["tools"]["completed"], 1);
    assert_eq!(v["data"]["fact_checks"]["total"], 1);
    assert_eq!(v["data"]["fact_checks"]["invalid_sources"], 0);
    assert_eq!(v["data"]["fact_checks"]["undigested_sources"], 0);
    assert_eq!(v["data"]["synthesis"]["completed"], 1);

    let events = v["data"]["events"].as_array().unwrap();
    assert!(events.len() >= 4, "timeline too small: {events:?}");
    for ev in events {
        assert!(ev.get("index").is_some(), "missing index: {ev}");
        assert!(ev.get("event").is_some(), "missing event: {ev}");
        assert!(ev.get("timestamp").is_some(), "missing timestamp: {ev}");
        assert!(ev.get("summary").is_some(), "missing summary: {ev}");
        assert!(
            !ev["summary"]
                .as_str()
                .unwrap_or_default()
                .contains("stdout"),
            "summary must not include raw stdout/stderr: {ev}"
        );
    }
}

#[test]
fn audit_reports_bilingual_html_status() {
    let env = Env::new();
    let slug = "audit-bilingual";
    env.new_session(slug, &[]);
    fs::write(
        env.session_dir(slug).join("report.html"),
        r#"<div class="lang-switch"><button data-mode="zh">中文</button></div><p>English.</p><p class="tr-zh" lang="zh-CN">中文。</p>"#,
    )
    .unwrap();
    env.append_jsonl(
        slug,
        &[
            synthesize_started_bilingual("codex"),
            synthesize_completed(),
        ],
    );

    let (v, code, stderr, _) = env.research(&["--json", "audit", slug]);
    assert_eq!(code, 0, "stderr={stderr}; envelope={v}");
    assert_eq!(v["data"]["audit_status"], "complete");
    assert_eq!(v["data"]["synthesis"]["bilingual_requested"], true);
    assert_eq!(v["data"]["synthesis"]["latest_bilingual_provider"], "codex");
    assert_eq!(v["data"]["synthesis"]["report_html"]["exists"], true);
    assert_eq!(v["data"]["synthesis"]["report_html"]["zh_paragraphs"], 1);
    assert_eq!(
        v["data"]["synthesis"]["report_html"]["language_switch"],
        "enabled"
    );
}

#[test]
fn audit_resolves_slug_prefixed_report_path_in_legacy_root() {
    let tmp = TempDir::new().unwrap();
    let slug = "legacy-audit";
    write_legacy_audit_session(tmp.path(), slug);

    let (v, code, stderr, _) = research_with_home(tmp.path(), &["--json", "audit", slug]);
    assert_eq!(code, 0, "stderr={stderr}; envelope={v}");
    assert_eq!(v["data"]["synthesis"]["report_html"]["exists"], true);
    assert_eq!(v["data"]["synthesis"]["report_html"]["zh_paragraphs"], 1);
    assert!(
        v["data"]["synthesis"]["report_html"]["path"]
            .as_str()
            .unwrap()
            .contains(".actionbook/research/legacy-audit/report.html"),
        "report path should resolve under legacy root: {v}"
    );
}

#[test]
fn audit_blocks_when_bilingual_requested_but_html_has_no_zh() {
    let env = Env::new();
    let slug = "audit-bilingual-missing";
    env.new_session(slug, &[]);
    fs::write(
        env.session_dir(slug).join("report.html"),
        r#"<div class="lang-switch"><button data-mode="zh" disabled>中文</button></div><p>English only.</p>"#,
    )
    .unwrap();
    env.append_jsonl(
        slug,
        &[
            synthesize_started_bilingual("claude"),
            synthesize_completed(),
        ],
    );

    let (v, code, stderr, _) = env.research(&["--json", "audit", slug]);
    assert_eq!(code, 0, "stderr={stderr}; envelope={v}");
    assert_eq!(v["data"]["audit_status"], "incomplete");
    assert_eq!(v["data"]["synthesis"]["bilingual_requested"], true);
    assert_eq!(v["data"]["synthesis"]["report_html"]["zh_paragraphs"], 0);
    assert_eq!(
        v["data"]["synthesis"]["report_html"]["language_switch"],
        "disabled"
    );
    let blockers = v["data"]["audit_blockers"].as_array().unwrap();
    assert!(array_contains_str(
        blockers,
        "bilingual_requested_but_no_zh_paragraphs"
    ));
}

#[test]
fn audit_detects_dangling_tool_call() {
    let env = Env::new();
    let slug = "audit-dangling";
    env.new_session(slug, &[]);
    env.append_jsonl(slug, &[tool_started("call-1")]);

    let (v, code, stderr, _) = env.research(&["--json", "audit", slug]);
    assert_eq!(code, 0, "stderr={stderr}; envelope={v}");
    assert_eq!(v["data"]["audit_status"], "incomplete");
    assert_eq!(v["data"]["tools"]["dangling"], 1);
    let blockers = v["data"]["audit_blockers"].as_array().unwrap();
    assert!(array_contains_str(blockers, "tool_calls_dangling"));
}

#[test]
fn audit_fact_check_invalid_source_surfaces_blocker() {
    let env = Env::new();
    let slug = "audit-invalid-fact";
    let accepted = "https://www.nba.com/lakers/roster";
    let missing = "https://missing.example/roster";
    env.new_session(slug, &["fact-check"]);
    env.append_jsonl(
        slug,
        &[
            source_accepted(accepted),
            source_digested(accepted),
            fact_checked(missing, "uncertain"),
            synthesize_completed(),
        ],
    );

    let (v, code, stderr, _) = env.research(&["--json", "audit", slug]);
    assert_eq!(code, 0, "stderr={stderr}; envelope={v}");
    assert_eq!(v["data"]["audit_status"], "incomplete");
    assert_eq!(v["data"]["fact_checks"]["invalid_sources"], 1);
    let blockers = v["data"]["audit_blockers"].as_array().unwrap();
    assert!(array_contains_str(blockers, "fact_check_invalid_sources"));
}

#[test]
fn audit_refuted_and_uncertain_fact_checks_block_completion() {
    let env = Env::new();
    let slug = "audit-bad-facts";
    let accepted = "https://www.nba.com/lakers/roster";
    env.new_session(slug, &["fact-check"]);
    env.append_jsonl(
        slug,
        &[
            source_accepted(accepted),
            source_digested(accepted),
            fact_checked(accepted, "refuted"),
            fact_checked(accepted, "uncertain"),
            synthesize_completed(),
        ],
    );

    let (v, code, stderr, _) = env.research(&["--json", "audit", slug]);
    assert_eq!(code, 0, "stderr={stderr}; envelope={v}");
    assert_eq!(v["data"]["audit_status"], "incomplete");
    assert_eq!(v["data"]["fact_checks"]["refuted"], 1);
    assert_eq!(v["data"]["fact_checks"]["uncertain"], 1);
    let blockers = v["data"]["audit_blockers"].as_array().unwrap();
    assert!(array_contains_str(blockers, "fact_checks_refuted"));
    assert!(array_contains_str(blockers, "fact_checks_uncertain"));
}

#[test]
fn audit_undigested_fact_check_source_blocks_completion() {
    let env = Env::new();
    let slug = "audit-undigested-fact";
    let accepted = "https://www.nba.com/lakers/roster";
    env.new_session(slug, &["fact-check"]);
    env.append_jsonl(
        slug,
        &[
            source_accepted(accepted),
            fact_checked(accepted, "supported"),
            synthesize_completed(),
        ],
    );

    let (v, code, stderr, _) = env.research(&["--json", "audit", slug]);
    assert_eq!(code, 0, "stderr={stderr}; envelope={v}");
    assert_eq!(v["data"]["audit_status"], "incomplete");
    assert_eq!(v["data"]["fact_checks"]["undigested_sources"], 1);
    let blockers = v["data"]["audit_blockers"].as_array().unwrap();
    assert!(array_contains_str(
        blockers,
        "fact_check_undigested_sources"
    ));
}

#[test]
fn audit_detects_orphan_completed_tool_call() {
    let env = Env::new();
    let slug = "audit-orphan-completed";
    env.new_session(slug, &[]);
    env.append_jsonl(
        slug,
        &[tool_completed("call-1", "ok"), synthesize_completed()],
    );

    let (v, code, stderr, _) = env.research(&["--json", "audit", slug]);
    assert_eq!(code, 0, "stderr={stderr}; envelope={v}");
    assert_eq!(v["data"]["audit_status"], "incomplete");
    assert_eq!(v["data"]["tools"]["orphan_completed"], 1);
    let blockers = v["data"]["audit_blockers"].as_array().unwrap();
    assert!(array_contains_str(blockers, "tool_calls_orphan_completed"));
}

#[test]
fn audit_does_not_append_session_events() {
    let env = Env::new();
    let slug = "audit-readonly";
    env.new_session(slug, &[]);
    env.append_jsonl(slug, &[tool_started("call-1")]);
    let before = fs::read_to_string(env.jsonl_path(slug)).unwrap();

    let (v, code, stderr, _) = env.research(&["--json", "audit", slug]);
    assert_eq!(code, 0, "stderr={stderr}; envelope={v}");
    let after = fs::read_to_string(env.jsonl_path(slug)).unwrap();
    assert_eq!(before, after, "audit must not append session events");
    assert!(
        !contains_file_named(&env.session_dir(slug), "2-nba-team-roster.json"),
        "audit must not create new raw artifacts"
    );
}

#[test]
fn skill_recommends_audit_after_synthesize() {
    let skill_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/ascent-research/SKILL.md");
    let skill = fs::read_to_string(skill_path).unwrap();
    assert!(
        skill.contains("ascent-research --json audit"),
        "skill must instruct agents to run audit after synthesize"
    );
    assert!(
        skill.contains("audit_status"),
        "skill must require final replies to surface audit_status"
    );
}
