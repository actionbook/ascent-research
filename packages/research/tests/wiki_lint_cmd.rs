//! Integration tests for `research wiki lint` — v3 Step 11.

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

    fn run(&self, args: &[&str]) -> (Value, i32, String) {
        let out = Command::new(binary())
            .args(args)
            .env("ACTIONBOOK_RESEARCH_HOME", &self.home)
            .output()
            .expect("spawn research binary");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let json_line = stdout.lines().find(|l| l.trim_start().starts_with('{'));
        let v: Value = match json_line {
            Some(line) => serde_json::from_str(line).unwrap_or(Value::Null),
            None => Value::Null,
        };
        (v, out.status.code().unwrap_or(-1), stderr)
    }

    fn seed(&self, slug: &str, pages: &[(&str, &str)]) {
        let (_, code, _) = self.run(&["new", slug, "--slug", slug, "--json"]);
        assert_eq!(code, 0);
        let wiki_dir = std::path::PathBuf::from(&self.home).join(slug).join("wiki");
        fs::create_dir_all(&wiki_dir).unwrap();
        for (page_slug, body) in pages {
            fs::write(wiki_dir.join(format!("{page_slug}.md")), body).unwrap();
        }
    }

    fn home_path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.home)
    }
}

#[test]
fn lint_reports_orphans_and_broken_links() {
    let env = Env::new();
    env.seed(
        "lint-1",
        &[
            ("a", "links [[ghost]] and [[b]]"),
            ("b", "body with no links"),
            ("orphan", "nothing points at me"),
        ],
    );
    let (v, code, _) = env.run(&["--json", "wiki", "lint", "--slug", "lint-1"]);
    assert_eq!(code, 0);
    assert_eq!(v["ok"], Value::Bool(true));

    let orphans = v["data"]["orphans"].as_array().unwrap();
    let orphan_slugs: Vec<&str> = orphans.iter().filter_map(|s| s.as_str()).collect();
    // `a` is orphan (nobody links to it), `orphan` is orphan. `b` has one
    // inbound from `a`, so NOT orphan.
    assert!(orphan_slugs.contains(&"orphan"));
    assert!(orphan_slugs.contains(&"a"));
    assert!(!orphan_slugs.contains(&"b"));

    let broken = v["data"]["broken_links"].as_array().unwrap();
    let targets: Vec<&str> = broken
        .iter()
        .filter_map(|b| b["to"].as_str())
        .collect();
    assert!(targets.contains(&"ghost"));

    assert!(v["data"]["issues"].as_u64().unwrap() > 0);
}

#[test]
fn lint_flags_stale_pages() {
    let env = Env::new();
    env.seed(
        "lint-stale",
        &[("old", "---\nupdated: 2020-01-01\n---\nbody"), ("fresh", "---\nupdated: 2099-01-01\n---\nbody")],
    );
    let (v, code, _) = env.run(&[
        "--json", "wiki", "lint", "--slug", "lint-stale", "--stale-days", "7",
    ]);
    assert_eq!(code, 0);
    let stale = v["data"]["stale"].as_array().unwrap();
    let slugs: Vec<&str> = stale.iter().filter_map(|s| s["slug"].as_str()).collect();
    assert_eq!(slugs, vec!["old"]);
}

#[test]
fn lint_logs_event_to_jsonl() {
    let env = Env::new();
    env.seed(
        "lint-log",
        &[("a", "body"), ("b", "links [[a]]")],
    );
    let (_, code, _) = env.run(&["--json", "wiki", "lint", "--slug", "lint-log"]);
    assert_eq!(code, 0);

    let jsonl = fs::read_to_string(env.home_path().join("lint-log").join("session.jsonl")).unwrap();
    assert!(
        jsonl.contains(r#""event":"wiki_lint_ran""#),
        "expected wiki_lint_ran event, got:\n{jsonl}"
    );
}

#[test]
fn lint_missing_crossrefs_flags_shared_source() {
    let env = Env::new();
    env.seed(
        "lint-xr",
        &[
            (
                "a",
                "---\nsources: [https://ex.com/x]\n---\nno link to b",
            ),
            (
                "b",
                "---\nsources: [https://ex.com/x]\n---\nno link to a",
            ),
        ],
    );
    let (v, code, _) = env.run(&["--json", "wiki", "lint", "--slug", "lint-xr"]);
    assert_eq!(code, 0);
    let missing = v["data"]["missing_crossrefs"].as_array().unwrap();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0]["shared_source"], "https://ex.com/x");
}

#[test]
fn lint_missing_session_errors() {
    let env = Env::new();
    let (v, code, _) = env.run(&["--json", "wiki", "lint", "--slug", "no-such"]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "SESSION_NOT_FOUND");
}
