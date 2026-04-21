//! Integration tests for `research wiki query` — v3 Step 10.
//!
//! Uses the `fake` provider via `ACTIONBOOK_FAKE_QUERY_RESPONSE` so the
//! suite never hits a real LLM. Only runs when the binary is built
//! with the `autoresearch` feature (same gate as the `loop` command).

#![cfg(feature = "autoresearch")]

use serde_json::Value;
use std::fs;
use std::process::Command;
use tempfile::TempDir;

fn binary() -> String {
    env!("CARGO_BIN_EXE_research").to_string()
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

    fn seed_session_with_wiki(&self, slug: &str, pages: &[(&str, &str)]) {
        let (_, code, _) = self.run(&["new", slug, "--slug", slug, "--json"]);
        assert_eq!(code, 0);
        let wiki_dir = self.home_path().join(slug).join("wiki");
        fs::create_dir_all(&wiki_dir).unwrap();
        for (page_slug, body) in pages {
            fs::write(wiki_dir.join(format!("{page_slug}.md")), body).unwrap();
        }
    }
}

#[test]
fn query_empty_question_errors() {
    let env = Env::new();
    env.seed_session_with_wiki(
        "q-empty",
        &[("scheduler", "---\nkind: concept\n---\n# Scheduler\nbody")],
    );
    let (v, code, _) = env.run(&[
        "--json",
        "wiki",
        "query",
        "",
        "--slug",
        "q-empty",
        "--provider",
        "fake",
    ]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "INVALID_ARGUMENT");
}

#[test]
fn query_empty_wiki_errors_with_wiki_empty() {
    let env = Env::new();
    let (_, _, _) = env.run(&["new", "q-noWiki", "--slug", "q-nowiki", "--json"]);
    let (v, code, _) = env.run(&[
        "--json",
        "wiki",
        "query",
        "anything",
        "--slug",
        "q-nowiki",
        "--provider",
        "fake",
    ]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "WIKI_EMPTY");
}

#[test]
fn query_returns_answer_and_logs_event() {
    let env = Env::new();
    env.seed_session_with_wiki(
        "q-ok",
        &[
            ("scheduler", "---\nkind: concept\n---\n# Scheduler\nBalances work across worker threads via stealing."),
            ("worker", "---\nkind: entity\n---\n# Worker\nRuns a queue of ready tasks."),
            ("misc", "---\nkind: concept\n---\n# Unrelated\nNothing."),
        ],
    );
    let (v, code, stderr) = env.run_with_env(
        &[
            "--json",
            "wiki",
            "query",
            "how does the scheduler balance worker load?",
            "--slug",
            "q-ok",
            "--provider",
            "fake",
        ],
        &[(
            "ACTIONBOOK_FAKE_QUERY_RESPONSE",
            "The scheduler balances via [[scheduler]] and [[worker]] stealing.",
        )],
    );
    assert_eq!(code, 0, "query must succeed (stderr: {stderr})");
    assert_eq!(v["ok"], Value::Bool(true));
    let answer = v["data"]["answer"].as_str().unwrap();
    assert!(answer.contains("[[scheduler]]"));
    let relevant = v["data"]["relevant_pages"].as_array().unwrap();
    let slugs: Vec<&str> = relevant.iter().filter_map(|v| v.as_str()).collect();
    assert!(slugs.contains(&"scheduler"), "scheduler must rank relevant, got {slugs:?}");
    // jsonl must contain a wiki_query event
    let jsonl = fs::read_to_string(env.home_path().join("q-ok").join("session.jsonl")).unwrap();
    assert!(
        jsonl.contains(r#""event":"wiki_query""#),
        "expected wiki_query event in jsonl, got:\n{jsonl}"
    );
}

#[test]
fn query_save_as_writes_analysis_page() {
    let env = Env::new();
    env.seed_session_with_wiki(
        "q-save",
        &[("scheduler", "---\nkind: concept\n---\n# Scheduler\nDescribes the scheduler.")],
    );
    let (v, code, _) = env.run_with_env(
        &[
            "--json",
            "wiki",
            "query",
            "how does the scheduler balance?",
            "--slug",
            "q-save",
            "--save-as",
            "scheduler-balancing",
            "--provider",
            "fake",
        ],
        &[(
            "ACTIONBOOK_FAKE_QUERY_RESPONSE",
            "Through [[scheduler]] work stealing.",
        )],
    );
    assert_eq!(code, 0);
    assert_eq!(v["ok"], Value::Bool(true));
    assert_eq!(v["data"]["answer_slug"], "scheduler-balancing");
    let saved_path = env
        .home_path()
        .join("q-save")
        .join("wiki")
        .join("scheduler-balancing.md");
    assert!(saved_path.exists(), "analysis page must be written");
    let body = fs::read_to_string(&saved_path).unwrap();
    assert!(body.starts_with("---\nkind: analysis\n"));
    assert!(body.contains("sources: [wiki:scheduler]"));
    assert!(body.contains("# scheduler-balancing"));
}

#[test]
fn query_invalid_format_errors() {
    let env = Env::new();
    env.seed_session_with_wiki(
        "q-fmt",
        &[("scheduler", "---\nkind: concept\n---\n# Scheduler\nbody")],
    );
    let (v, code, _) = env.run(&[
        "--json",
        "wiki",
        "query",
        "q?",
        "--slug",
        "q-fmt",
        "--format",
        "freeform",
        "--provider",
        "fake",
    ]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "INVALID_ARGUMENT");
}
