//! Integration tests for research-synthesize.spec.md scenarios.
//!
//! Since the json-ui dependency was removed, `research synthesize` now
//! renders `report.html` in-process via the rich-html template pipeline
//! (shared with `research report --format rich-html`). Tests touch the
//! real renderer; no fake binaries required.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
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
        self.research_with_env(args, &[])
    }

    fn research_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> (Value, i32, String) {
        let mut cmd = Command::new(research_bin());
        cmd.args(args);
        cmd.env("ACTIONBOOK_RESEARCH_HOME", &self.home);
        // Default: skip `--open` side effects even if tests forget.
        cmd.env("SYNTHESIZE_NO_OPEN", "1");
        for (key, value) in envs {
            cmd.env(key, value);
        }
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

fn accepted_event(ts: &str, url: &str, kind: &str, bytes: u64) -> String {
    format!(
        r#"{{"event":"source_accepted","timestamp":"{ts}","url":"{url}","kind":"{kind}","executor":"postagent","raw_path":"raw/x.json","bytes":{bytes},"trust_score":2.0}}"#
    )
}

fn write_session_md(dir: &Path, body: &str) {
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

fn ready_md(url: &str) -> String {
    let overview = "Overview body with enough content to satisfy the coverage gate. ".repeat(6);
    format!(
        "\
# Research: T

## Overview
{overview}

## Findings
### Finding A
Body for A.

### Finding B
Body for B.

## Notes
Long-form analysis here, grounded by [the accepted source]({url}).

## 01 · A
Body a with a grounded claim from [the accepted source]({url}).

## 02 · B
Body b.

## 03 · C
Body c with a diagram.

![f](diagrams/g.svg)

## Sources
<!-- research:sources-start -->
<!-- research:sources-end -->
"
    )
}

fn append_jsonl_line(dir: &Path, line: &str) {
    let path = dir.join("session.jsonl");
    let mut current = fs::read_to_string(&path).unwrap_or_default();
    current.push_str(line);
    current.push('\n');
    fs::write(path, current).unwrap();
}

fn prep_report_ready_session(env: &Env, slug: &str) {
    env.research(&["new", "topic", "--slug", slug, "--json"]);
    let dir = env.session_dir(slug);
    let url = "https://ok.test/";
    write_session_md(&dir, &ready_md(url));
    let diag = dir.join("diagrams");
    fs::create_dir_all(&diag).unwrap();
    fs::write(diag.join("g.svg"), "<svg/>").unwrap();
    append_jsonl_line(&dir, &accepted_event("2026-04-20T10:00:00Z", url, "k", 1));
}

fn prep_fact_check_tagged_report_ready_session(env: &Env, slug: &str) {
    env.research(&[
        "new",
        "topic",
        "--slug",
        slug,
        "--tag",
        "fact-check",
        "--json",
    ]);
    let dir = env.session_dir(slug);
    let url = "https://ok.test/";
    write_session_md(&dir, &ready_md(url));
    let diag = dir.join("diagrams");
    fs::create_dir_all(&diag).unwrap();
    fs::write(diag.join("g.svg"), "<svg/>").unwrap();
    append_jsonl_line(&dir, &accepted_event("2026-04-20T10:00:00Z", url, "k", 1));
}

#[test]
fn synthesize_happy_path_writes_json_and_html() {
    let env = Env::new();
    prep_report_ready_session(&env, "s1");

    let (v, code, stderr) = env.research(&["synthesize", "s1", "--json"]);
    assert_eq!(code, 0, "stderr: {stderr}; v={v}");
    assert_eq!(v["data"]["accepted_sources"], 1);
    assert_eq!(v["data"]["rejected_sources"], 0);
    assert_eq!(v["data"]["bilingual"]["requested"], false);
    assert_eq!(v["data"]["bilingual"]["status"], "not_requested");
    assert_eq!(v["data"]["bilingual"]["zh_paragraphs"], 0);
    assert_eq!(v["data"]["pdf"]["requested"], false);
    assert_eq!(v["data"]["pdf"]["status"], "not_requested");
    assert!(v["data"]["pdf"]["report_pdf_path"].is_null());
    assert!(env.session_dir("s1").join("report.json").exists());
    assert!(env.session_dir("s1").join("report.html").exists());
    assert!(!env.session_dir("s1").join("report.pdf").exists());

    let jsonl = fs::read_to_string(env.session_dir("s1").join("session.jsonl")).unwrap();
    assert!(jsonl.contains("synthesize_started"));
    assert!(jsonl.contains("synthesize_completed"));
}

#[test]
fn synthesize_pdf_is_explicit_and_uses_local_chrome_by_default() {
    let env = Env::new();
    prep_report_ready_session(&env, "s1pdf-local");
    let fake_chrome = env.session_dir("s1pdf-local").join("fake-chrome.sh");
    fs::write(
        &fake_chrome,
        r#"#!/bin/sh
out=""
for arg in "$@"; do
  case "$arg" in
    --print-to-pdf=*) out="${arg#--print-to-pdf=}" ;;
  esac
done
test -n "$out" || exit 3
printf '%s\n' '%PDF-1.4 fake local' > "$out"
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake_chrome, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let fake_chrome_s = fake_chrome.to_string_lossy().into_owned();
    let (v, code, stderr) = env.research_with_env(
        &["synthesize", "s1pdf-local", "--pdf", "--json"],
        &[("ASR_PDF_CHROME_BIN", &fake_chrome_s)],
    );
    assert_eq!(code, 0, "stderr: {stderr}; v={v}");
    assert_eq!(v["data"]["pdf"]["requested"], true);
    assert_eq!(v["data"]["pdf"]["provider"], "local");
    assert_eq!(v["data"]["pdf"]["status"], "complete");
    assert_eq!(
        v["data"]["pdf"]["report_pdf_path"],
        "s1pdf-local/report.pdf"
    );
    assert!(env.session_dir("s1pdf-local").join("report.pdf").exists());
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
    prep_report_ready_session(&env, "s3");

    let (v, code, _) = env.research(&["synthesize", "s3", "--no-render", "--json"]);
    assert_eq!(code, 0);
    assert!(env.session_dir("s3").join("report.json").exists());
    assert!(!env.session_dir("s3").join("report.html").exists());
    assert!(v["data"]["report_html_path"].is_null());
}

#[test]
fn synthesize_report_has_canonical_structure() {
    let env = Env::new();
    prep_report_ready_session(&env, "s5");
    env.research(&["synthesize", "s5", "--json"]);

    let text = fs::read_to_string(env.session_dir("s5").join("report.json")).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["type"], "Report");
    let children = v["children"].as_array().unwrap();
    let types: Vec<&str> = children
        .iter()
        .map(|c| c["type"].as_str().unwrap())
        .collect();
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
    assert!(
        html.contains("Instrument Serif"),
        "rich-html template signature"
    );
    assert!(
        html.contains("#f7591f") || html.contains("f7591f"),
        "accent color token"
    );
}

#[test]
fn synthesize_is_idempotent_rewrite() {
    let env = Env::new();
    prep_report_ready_session(&env, "s6");
    let args: &[&str] = &["synthesize", "s6", "--json"];

    let (_, code1, _) = env.research(args);
    assert_eq!(code1, 0);
    let first = fs::metadata(env.session_dir("s6").join("report.json")).unwrap();

    // Rewrite findings to have only 1 entry.
    let modified = "\
# Research: T

## Overview
Overview body with enough content to satisfy the coverage gate. Overview body with enough content to satisfy the coverage gate. Overview body with enough content to satisfy the coverage gate. Overview body with enough content to satisfy the coverage gate. Overview body with enough content to satisfy the coverage gate. Overview body with enough content to satisfy the coverage gate.

## Findings
### Only One
The one finding.

## Notes
Notes grounded by [the accepted source](https://ok.test/).

## 01 · A
Body a with a grounded claim from [the accepted source](https://ok.test/).

## 02 · B
Body b.

## 03 · C
Body c with a diagram.

![f](diagrams/g.svg)

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
    let items = findings["children"][0]["props"]["items"]
        .as_array()
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "Only One");
}

#[test]
fn synthesize_rejects_session_when_coverage_not_ready() {
    let env = Env::new();
    env.research(&["new", "topic", "--slug", "s7", "--json"]);
    write_session_md(&env.session_dir("s7"), sample_md());

    let (v, code, _) = env.research(&["synthesize", "s7", "--json"]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "REPORT_NOT_READY");
    let blockers = v["error"]["details"]["report_ready_blockers"]
        .as_array()
        .unwrap();
    assert!(
        blockers
            .iter()
            .any(|b| b.as_str().unwrap().contains("sources_accepted"))
    );
    assert!(!env.session_dir("s7").join("report.json").exists());
    assert!(!env.session_dir("s7").join("report.html").exists());
}

#[test]
fn synthesize_rejects_fact_check_tag_without_fact_check() {
    let env = Env::new();
    prep_fact_check_tagged_report_ready_session(&env, "s8");

    let (v, code, _) = env.research(&["synthesize", "s8", "--json"]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "REPORT_NOT_READY");
    let blockers = v["error"]["details"]["report_ready_blockers"]
        .as_array()
        .unwrap();
    assert!(
        blockers
            .iter()
            .any(|b| b.as_str().unwrap().contains("fact_checks_total")),
        "expected fact-check blocker; got: {blockers:?}"
    );
    assert!(!env.session_dir("s8").join("report.html").exists());
}

#[test]
fn skill_requires_fact_check_for_dynamic_topics() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let skill_path = repo_root.join("skills/ascent-research/SKILL.md");
    let skill = fs::read_to_string(&skill_path).expect("read ascent-research skill");
    let lower = skill.to_lowercase();

    assert!(
        skill.contains("--tag fact-check"),
        "skill must require fact-check tag for dynamic topics"
    );
    assert!(skill.contains("fact_checks_total"));
    assert!(skill.contains("fact_check"));
    assert!(lower.contains("live"));
    assert!(lower.contains("sports"));
    assert!(lower.contains("news"));
    assert!(lower.contains("current roster"));
    assert!(lower.contains("current price"));
}
