//! Integration tests for research-synthesize.spec.md scenarios.
//!
//! Since the json-ui dependency was removed, `research synthesize` now
//! renders `report.html` in-process via the rich-html template pipeline
//! (shared with `research report --format rich-html`). Tests touch the
//! real renderer; no fake binaries required.

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
        // Default: skip `--open` side effects even if tests forget.
        cmd.env("SYNTHESIZE_NO_OPEN", "1");
        let out = cmd.output().expect("spawn research");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let json_line = stdout.lines().find(|l| l.trim_start().starts_with('{'));
        let v: Value = match json_line {
            Some(l) => serde_json::from_str(l).unwrap_or(Value::Null),
            None => Value::Null,
        };
        (v, out.status.code().unwrap_or(-1), stderr)
    }

    fn session_dir(&self, slug: &str) -> PathBuf {
        PathBuf::from(&self.home).join(slug)
    }
}

fn write_session_md(dir: &PathBuf, body: &str) {
    fs::write(dir.join("session.md"), body).unwrap();
}

fn sample_md() -> &'static str {
    "\
# Research: T

## Overview
Overview body with enough content to not be a placeholder.

## Findings
### Finding A
Body for A.

### Finding B
Body for B.

## Notes
Long-form analysis here.

## Sources
<!-- research:sources-start -->
<!-- research:sources-end -->
"
}

#[test]
fn synthesize_happy_path_writes_json_and_html() {
    let env = Env::new();
    env.research(&["new", "topic", "--slug", "s1", "--json"]);
    write_session_md(&env.session_dir("s1"), sample_md());

    let (v, code, stderr) = env.research(&["synthesize", "s1", "--json"]);
    assert_eq!(code, 0, "stderr: {stderr}; v={v}");
    assert_eq!(v["data"]["accepted_sources"], 0);
    assert_eq!(v["data"]["rejected_sources"], 0);
    assert!(env.session_dir("s1").join("report.json").exists());
    assert!(env.session_dir("s1").join("report.html").exists());

    let jsonl = fs::read_to_string(env.session_dir("s1").join("session.jsonl")).unwrap();
    assert!(jsonl.contains("synthesize_started"));
    assert!(jsonl.contains("synthesize_completed"));
}

#[test]
fn synthesize_missing_overview_is_fatal() {
    let env = Env::new();
    env.research(&["new", "t2", "--slug", "s2", "--json"]);
    // md template has placeholder Overview → should be treated as missing
    let (v, code, _) = env.research(&["synthesize", "s2", "--json"]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "MISSING_OVERVIEW");
}

#[test]
fn synthesize_no_render_skips_html() {
    let env = Env::new();
    env.research(&["new", "t3", "--slug", "s3", "--json"]);
    write_session_md(&env.session_dir("s3"), sample_md());

    let (v, code, _) = env.research(&["synthesize", "s3", "--no-render", "--json"]);
    assert_eq!(code, 0);
    assert!(env.session_dir("s3").join("report.json").exists());
    assert!(!env.session_dir("s3").join("report.html").exists());
    assert!(v["data"]["report_html_path"].is_null());
}

#[test]
fn synthesize_report_has_canonical_structure() {
    let env = Env::new();
    env.research(&["new", "t5", "--slug", "s5", "--json"]);
    write_session_md(&env.session_dir("s5"), sample_md());
    env.research(&["synthesize", "s5", "--json"]);

    let text = fs::read_to_string(env.session_dir("s5").join("report.json")).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["type"], "Report");
    let children = v["children"].as_array().unwrap();
    let types: Vec<&str> = children.iter().map(|c| c["type"].as_str().unwrap()).collect();
    assert!(types.contains(&"BrandHeader"));
    assert!(types.contains(&"BrandFooter"));
    let titles: Vec<&str> = children
        .iter()
        .filter_map(|c| c["props"]["title"].as_str())
        .collect();
    assert!(titles.contains(&"Overview"));
    assert!(titles.contains(&"Key Findings"));
    assert!(titles.contains(&"Analysis"));
    assert!(titles.contains(&"Sources"));
    assert!(titles.contains(&"Methodology"));

    // rich-html render produced the HTML too (shared pipeline).
    let html = fs::read_to_string(env.session_dir("s5").join("report.html")).unwrap();
    assert!(html.contains("Instrument Serif"), "rich-html template signature");
    assert!(html.contains("#f7591f") || html.contains("f7591f"), "accent color token");
}

#[test]
fn synthesize_is_idempotent_rewrite() {
    let env = Env::new();
    env.research(&["new", "t6", "--slug", "s6", "--json"]);
    write_session_md(&env.session_dir("s6"), sample_md());
    let args: &[&str] = &["synthesize", "s6", "--json"];

    let (_, code1, _) = env.research(args);
    assert_eq!(code1, 0);
    let first = fs::metadata(env.session_dir("s6").join("report.json")).unwrap();

    // Rewrite findings to have only 1 entry.
    let modified = "\
# Research: T

## Overview
Overview body with enough content to not be a placeholder.

## Findings
### Only One
The one finding.

## Notes
Notes.

## Sources
<!-- research:sources-start -->
<!-- research:sources-end -->
";
    write_session_md(&env.session_dir("s6"), modified);

    let (_, code2, _) = env.research(args);
    assert_eq!(code2, 0);
    let second = fs::metadata(env.session_dir("s6").join("report.json")).unwrap();
    // modification time should advance (or at least not be earlier)
    assert!(second.modified().unwrap() >= first.modified().unwrap());

    // Content reflects 1 finding, not 2
    let text = fs::read_to_string(env.session_dir("s6").join("report.json")).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    let children = v["children"].as_array().unwrap();
    let findings = children
        .iter()
        .find(|c| c["props"]["title"] == "Key Findings")
        .unwrap();
    let items = findings["children"][0]["props"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "Only One");
}
