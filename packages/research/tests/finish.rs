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
        let out = Command::new(research_bin())
            .args(args)
            .env("ACTIONBOOK_RESEARCH_HOME", &self.home)
            .env("SYNTHESIZE_NO_OPEN", "1")
            .output()
            .expect("spawn ascent-research");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let json_line = stdout.lines().find(|l| l.trim_start().starts_with('{'));
        let v: Value = match json_line {
            Some(line) => serde_json::from_str(line).unwrap_or(Value::Null),
            None => Value::Null,
        };
        (v, out.status.code().unwrap_or(-1), stderr)
    }

    fn new_session(&self, slug: &str, topic: &str) {
        let (v, code, stderr) = self.research(&["--json", "new", topic, "--slug", slug]);
        assert_eq!(code, 0, "new failed: stderr={stderr}; envelope={v}");
    }

    fn session_dir(&self, slug: &str) -> PathBuf {
        PathBuf::from(&self.home).join(slug)
    }

    fn write_session_md(&self, slug: &str, body: &str) {
        fs::write(self.session_dir(slug).join("session.md"), body).unwrap();
    }

    fn write_diagram(&self, slug: &str, name: &str, body: &str) {
        let dir = self.session_dir(slug).join("diagrams");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), body).unwrap();
    }

    fn write_raw(&self, slug: &str, path: &str, body: &str) {
        let path = self.session_dir(slug).join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn append_event(&self, slug: &str, line: &str) {
        let path = self.session_dir(slug).join("session.jsonl");
        let mut current = fs::read_to_string(&path).unwrap_or_default();
        current.push_str(line);
        current.push('\n');
        fs::write(path, current).unwrap();
    }
}

fn prep_ready_session(env: &Env, slug: &str) {
    env.new_session(slug, "Ready finish session");
    env.write_session_md(
        slug,
        r#"## Overview
This overview is intentionally long enough to satisfy the coverage gate. It describes the project, evidence, method, and outcome in enough detail for the report builder to accept it as real content rather than placeholder prose.

## 01 · WHY
Body cites [the accepted source](https://example.com/source).

## 02 · HOW
![diagram](diagrams/g.svg)

## 03 · WHAT
More analysis.
"#,
    );
    env.write_diagram(
        slug,
        "g.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#,
    );
    env.append_event(
        slug,
        r#"{"event":"source_accepted","timestamp":"2026-04-24T00:00:00Z","url":"https://example.com/source","kind":"browser","executor":"browser","raw_path":"raw/1-source.txt","bytes":100,"trust_score":1.0}"#,
    );
    env.write_raw(slug, "raw/1-source.txt", "source body");
}

#[test]
fn finish_runs_coverage_synthesize_and_audit() {
    let env = Env::new();
    prep_ready_session(&env, "ready");

    let (v, code, stderr) = env.research(&["--json", "finish", "ready"]);
    assert_eq!(code, 0, "stderr={stderr}; envelope={v}");
    assert_eq!(v["data"]["coverage"]["report_ready"], true);
    assert!(
        v["data"]["synthesis"]["report_html_path"]
            .as_str()
            .unwrap()
            .ends_with("report.html")
    );
    assert_eq!(v["data"]["audit"]["audit_status"], "complete");
}

#[test]
fn finish_stops_before_synthesize_when_coverage_not_ready() {
    let env = Env::new();
    env.new_session("not-ready", "Not ready");

    let (v, code, _) = env.research(&["--json", "finish", "not-ready"]);
    assert_ne!(code, 0, "{v}");
    assert_eq!(v["error"]["code"], "REPORT_NOT_READY");
    assert_eq!(v["error"]["details"]["stage"], "coverage");
    assert!(!env.session_dir("not-ready").join("report.html").exists());
}

#[test]
fn finish_fails_when_audit_incomplete() {
    let env = Env::new();
    prep_ready_session(&env, "dangling");
    env.append_event(
        "dangling",
        r#"{"event":"tool_call_started","timestamp":"2026-04-24T00:00:00Z","call_id":"c1","hand":"postagent","tool":"postagent send","input_summary":"x"}"#,
    );

    let (v, code, _) = env.research(&["--json", "finish", "dangling"]);
    assert_ne!(code, 0, "{v}");
    assert_eq!(v["error"]["details"]["stage"], "audit");
    assert_eq!(v["error"]["details"]["audit"]["audit_status"], "incomplete");
    assert!(
        v["error"]["details"]["audit"]["audit_blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b.as_str().unwrap().contains("tool_calls_dangling"))
    );
}

#[test]
fn finish_keeps_inspection_commands_independent() {
    let env = Env::new();
    prep_ready_session(&env, "independent");

    assert_eq!(env.research(&["--json", "coverage", "independent"]).1, 0);
    assert_eq!(env.research(&["--json", "synthesize", "independent"]).1, 0);
    assert_eq!(env.research(&["--json", "audit", "independent"]).1, 0);
}

#[test]
fn skill_recommends_finish_for_mandatory_tail() {
    let skill_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../skills/ascent-research/SKILL.md");
    let skill = fs::read_to_string(skill_path).unwrap();
    assert!(skill.contains("ascent-research finish"));
    assert!(skill.contains("coverage"));
    assert!(skill.contains("synthesize"));
    assert!(skill.contains("audit"));
}
