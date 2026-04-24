//! Integration tests for `research schema {show, edit}` — v3 Step 9.
//!
//! These hit the real `research` binary with `ACTIONBOOK_RESEARCH_HOME`
//! pointed at a fresh tempdir, so slug resolution, envelope rendering,
//! and jsonl writes are all verified end-to-end.

use serde_json::Value;
use std::fs;
use std::process::Command;
use tempfile::TempDir;

fn binary() -> String {
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

    fn run_with_env(&self, args: &[&str], extra_env: &[(&str, &str)]) -> (Value, i32, String) {
        let mut cmd = Command::new(binary());
        cmd.args(args).env("ACTIONBOOK_RESEARCH_HOME", &self.home);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn research binary");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let json_line = stdout.lines().find(|l| l.trim_start().starts_with('{'));
        let v: Value = match json_line {
            Some(line) => serde_json::from_str(line).unwrap_or(Value::Null),
            None => Value::Null,
        };
        (v, out.status.code().unwrap_or(-1), stderr)
    }

    fn run(&self, args: &[&str]) -> (Value, i32, String) {
        self.run_with_env(args, &[])
    }

    fn home_path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.home)
    }
}

#[test]
fn new_seeds_starter_schema() {
    let env = Env::new();
    let (_, code, _) = env.run(&["new", "schema smoke", "--slug", "sch-new", "--json"]);
    assert_eq!(code, 0, "research new should succeed");

    let schema_path = env.home_path().join("sch-new").join("SCHEMA.md");
    assert!(
        schema_path.exists(),
        "SCHEMA.md must be seeded on `research new`"
    );
    let body = fs::read_to_string(&schema_path).unwrap();
    for section in ["## Goal", "## Wiki conventions", "## House style"] {
        assert!(
            body.contains(section),
            "starter SCHEMA.md missing {section}"
        );
    }
}

#[test]
fn schema_show_reports_existing_file() {
    let env = Env::new();
    let (_, _, _) = env.run(&["new", "show-test", "--slug", "sch-show", "--json"]);

    let (v, code, _) = env.run(&["--json", "schema", "show", "--slug", "sch-show"]);
    assert_eq!(code, 0);
    assert_eq!(v["ok"], Value::Bool(true));
    assert_eq!(v["data"]["exists"], Value::Bool(true));
    let body = v["data"]["body"].as_str().unwrap();
    assert!(body.contains("## Goal"));
}

#[test]
fn schema_show_reports_missing_without_error() {
    // If a session exists but SCHEMA.md was manually deleted, `show` must
    // still return ok=true with exists=false — never a hard failure.
    let env = Env::new();
    let (_, _, _) = env.run(&["new", "missing", "--slug", "sch-miss", "--json"]);
    let schema_path = env.home_path().join("sch-miss").join("SCHEMA.md");
    fs::remove_file(&schema_path).unwrap();

    let (v, code, _) = env.run(&["--json", "schema", "show", "--slug", "sch-miss"]);
    assert_eq!(code, 0, "missing SCHEMA.md should not fail `show`");
    assert_eq!(v["ok"], Value::Bool(true));
    assert_eq!(v["data"]["exists"], Value::Bool(false));
    assert_eq!(v["data"]["bytes"], 0);
}

#[test]
fn schema_show_errors_on_missing_session() {
    let env = Env::new();
    let (v, code, _) = env.run(&["--json", "schema", "show", "--slug", "no-such-session"]);
    assert_ne!(code, 0);
    assert_eq!(v["ok"], Value::Bool(false));
    assert_eq!(v["error"]["code"], "SESSION_NOT_FOUND");
}

#[test]
fn schema_edit_writes_and_logs_schema_updated() {
    // Use a fake EDITOR that appends a marker line, so the "edit" hook
    // can detect a real mtime/body change and emit SchemaUpdated.
    let env = Env::new();
    let (_, _, _) = env.run(&["new", "edit-test", "--slug", "sch-edit", "--json"]);

    // Fake editor: `sh -c "echo MARKER >> $1"` — takes path as arg.
    let fake_editor = "sh -c 'echo \"\n## user-added section\n\" >> \"$1\"' --";

    let (v, code, stderr) = env.run_with_env(
        &["--json", "schema", "edit", "--slug", "sch-edit"],
        &[("EDITOR", fake_editor)],
    );
    assert_eq!(code, 0, "schema edit should succeed (stderr: {stderr})");
    assert_eq!(v["ok"], Value::Bool(true));
    assert_eq!(v["data"]["changed"], Value::Bool(true));

    // jsonl must now contain a SchemaUpdated event.
    let jsonl = fs::read_to_string(env.home_path().join("sch-edit").join("session.jsonl")).unwrap();
    assert!(
        jsonl.contains(r#""event":"schema_updated""#),
        "jsonl missing schema_updated event; got:\n{jsonl}"
    );
}

#[test]
fn schema_edit_no_change_no_event() {
    // Editor that's a no-op — SchemaUpdated must NOT be emitted when
    // nothing changed, so we don't spam the log on accidental :q.
    let env = Env::new();
    let (_, _, _) = env.run(&["new", "noedit", "--slug", "sch-noedit", "--json"]);

    let (v, code, _) = env.run_with_env(
        &["--json", "schema", "edit", "--slug", "sch-noedit"],
        &[("EDITOR", "true")],
    );
    assert_eq!(code, 0);
    assert_eq!(v["data"]["changed"], Value::Bool(false));

    let jsonl =
        fs::read_to_string(env.home_path().join("sch-noedit").join("session.jsonl")).unwrap();
    assert!(
        !jsonl.contains(r#""event":"schema_updated""#),
        "should not emit schema_updated when nothing changed"
    );
}
