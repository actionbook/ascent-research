//! Integration tests for research-report-templates.spec.md scenarios.
//!
//! Exec the built binary end-to-end so we cover:
//! - clap --format parsing
//! - envelope shape
//! - template + markdown + sources wiring together
//! - output file actually landing on disk

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

fn research_bin() -> String {
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

    fn research(&self, args: &[&str]) -> (Value, i32, String, String) {
        let mut cmd = Command::new(research_bin());
        cmd.args(args);
        cmd.env("ACTIONBOOK_RESEARCH_HOME", &self.home);
        cmd.env("RESEARCH_NO_OPEN", "1");
        let out = cmd.output().expect("spawn research");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let json_line = stdout.lines().find(|l| l.trim_start().starts_with('{'));
        let v: Value = match json_line {
            Some(l) => serde_json::from_str(l).unwrap_or(Value::Null),
            None => Value::Null,
        };
        (v, out.status.code().unwrap_or(-1), stderr, stdout)
    }

    fn session_dir(&self, slug: &str) -> PathBuf {
        PathBuf::from(&self.home).join(slug)
    }

    /// Create a session via `research new`, then rewrite session.md so the
    /// test controls Overview / Findings / asides / diagrams.
    fn prep_session(&self, slug: &str, body: &str) {
        let (v, code, stderr, _) = self.research(&["new", "topic", "--slug", slug, "--json"]);
        assert_eq!(code, 0, "new failed: {stderr}; v={v}");
        fs::write(self.session_dir(slug).join("session.md"), body).unwrap();
    }

    /// Append raw jsonl lines to session.jsonl. Bypasses `research add` so
    /// tests don't need the network / postagent.
    fn append_jsonl(&self, slug: &str, lines: &[&str]) {
        let path = self.session_dir(slug).join("session.jsonl");
        let mut current = fs::read_to_string(&path).unwrap_or_default();
        for l in lines {
            current.push_str(l);
            current.push('\n');
        }
        fs::write(&path, current).unwrap();
    }
}

fn accepted_event(ts: &str, url: &str, kind: &str, bytes: u64) -> String {
    format!(
        r#"{{"event":"source_accepted","timestamp":"{ts}","url":"{url}","kind":"{kind}","executor":"postagent","raw_path":"raw/x.json","bytes":{bytes},"trust_score":2.0}}"#
    )
}

// ── Spec acceptance #1: happy-path rich-html ───────────────────────────────

#[test]
fn happy_path_rich_html() {
    let env = Env::new();
    // Diagram file
    let body = r#"## Overview
Browser-harness flips the usual framework stance.

> **aside:** The less you build, the more it works.

## 01 · WHY
Frameworks grow friction.

## 02 · WHAT
Four Python files totaling ~592 lines.

![Fig · demo](diagrams/foo.svg)
"#;
    env.prep_session("s1", body);
    // Accepted sources
    env.append_jsonl(
        "s1",
        &[
            &accepted_event("2026-04-19T10:01:00Z", "https://example.com/a", "github-file", 100),
            &accepted_event("2026-04-19T10:02:00Z", "https://example.com/b", "github-tree", 200),
        ],
    );
    // Diagram
    let diag_dir = env.session_dir("s1").join("diagrams");
    fs::create_dir_all(&diag_dir).unwrap();
    fs::write(
        diag_dir.join("foo.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\"><circle r=\"5\"/></svg>",
    )
    .unwrap();

    let (v, code, stderr, _) = env.research(&[
        "report", "s1", "--format", "rich-html", "--no-open", "--json",
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(v["data"]["format"], "rich-html");
    assert_eq!(v["data"]["sources_count"], 2);
    assert_eq!(v["data"]["diagrams_inlined"], 1);
    assert_eq!(v["data"]["phase"], "C");

    let report = fs::read_to_string(env.session_dir("s1").join("report-rich.html")).unwrap();
    assert!(report.len() > 1000);
    // Aside — exactly once
    assert_eq!(report.matches("<p class=\"aside\">").count(), 1);
    // SVG inlined
    assert!(report.contains("<circle r=\"5\"/>"));
    // Section numbers
    assert!(report.contains("<span class=\"section-num\">01</span>"));
    assert!(report.contains("<span class=\"section-num\">02</span>"));
    // All sources clickable
    assert!(report.contains("href=\"https://example.com/a\""));
    assert!(report.contains("href=\"https://example.com/b\""));
}

// ── Spec acceptance #2: MISSING_OVERVIEW fatal ─────────────────────────────

#[test]
fn missing_overview_is_fatal() {
    let env = Env::new();
    env.prep_session(
        "s2",
        "## Overview\n<!-- placeholder only -->\n\n## Findings\nx\n",
    );
    let (v, code, _, _) = env.research(&[
        "report", "s2", "--format", "rich-html", "--no-open", "--json",
    ]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "MISSING_OVERVIEW");
}

// ── Spec acceptance #3: multiple asides → warning + first wins ─────────────

#[test]
fn multiple_asides_warn_and_keep_first() {
    let env = Env::new();
    env.prep_session(
        "s3",
        "## Overview\nbody\n\n> **aside:** first one\n\ntext\n\n> **aside:** second one\n",
    );
    let (v, code, _, _) = env.research(&[
        "report", "s3", "--format", "rich-html", "--no-open", "--json",
    ]);
    assert_eq!(code, 0);

    let report = fs::read_to_string(env.session_dir("s3").join("report-rich.html")).unwrap();
    assert_eq!(
        report.matches("<p class=\"aside\">").count(),
        1,
        "exactly one aside renders"
    );
    let warnings = v["data"]["warnings"].as_array().unwrap();
    assert!(warnings.iter().any(|w| w.as_str() == Some("aside_multiple")));
}

// ── Spec acceptance #4: DIAGRAM_OUT_OF_BOUNDS ─────────────────────────────

#[test]
fn diagram_out_of_bounds_rejected() {
    let env = Env::new();
    env.prep_session(
        "s4",
        "## Overview\nfoo\n\n![bad](diagrams/../../etc/passwd.svg)\n",
    );
    let (v, code, _, _) = env.research(&[
        "report", "s4", "--format", "rich-html", "--no-open", "--json",
    ]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "DIAGRAM_OUT_OF_BOUNDS");
}

// ── Spec acceptance #5: diagram missing → <img> fallback + warning ─────────

#[test]
fn diagram_missing_falls_back_to_img() {
    let env = Env::new();
    env.prep_session(
        "s5",
        "## Overview\nbody\n\n![gone](diagrams/missing.svg)\n",
    );
    // Ensure diagrams/ dir exists so canonicalize on parent resolves the way
    // resolve_diagram expects.
    fs::create_dir_all(env.session_dir("s5").join("diagrams")).unwrap();
    let (v, code, _, _) = env.research(&[
        "report", "s5", "--format", "rich-html", "--no-open", "--json",
    ]);
    assert_eq!(code, 0);

    let report = fs::read_to_string(env.session_dir("s5").join("report-rich.html")).unwrap();
    assert!(
        report.contains("<img src=\"diagrams/missing.svg\""),
        "expected <img> fallback"
    );
    let warnings = v["data"]["warnings"].as_array().unwrap();
    assert!(warnings.iter().any(|w| w.as_str() == Some("diagram_fallback_img")));
}

// ── Spec acceptance #6: `## 01 · WHY` heading → badge span ────────────────

#[test]
fn section_numbers_render_as_badge() {
    let env = Env::new();
    env.prep_session(
        "s6",
        "## Overview\nhello\n\n## 01 · WHY\nintro\n\n## 02 · WHAT\nbody\n",
    );
    let (_, code, _, _) = env.research(&[
        "report", "s6", "--format", "rich-html", "--no-open", "--json",
    ]);
    assert_eq!(code, 0);

    let report = fs::read_to_string(env.session_dir("s6").join("report-rich.html")).unwrap();
    assert!(report.contains("<span class=\"section-num\">01</span><span>WHY</span>"));
    assert!(report.contains("<span class=\"section-num\">02</span><span>WHAT</span>"));
    // Plain "01 · WHY" text must not survive into the heading.
    assert!(!report.contains("<h2>01 · WHY</h2>"));
}

// ── Spec acceptance #7: sources come from jsonl, not md block ─────────────

#[test]
fn sources_from_jsonl_not_md() {
    let env = Env::new();
    env.prep_session("s7", "## Overview\nx\n");

    // Three accepted events in jsonl
    env.append_jsonl(
        "s7",
        &[
            &accepted_event("2026-04-19T10:01:00Z", "https://a.test/", "kindA", 1),
            &accepted_event("2026-04-19T10:02:00Z", "https://b.test/", "kindB", 2),
            &accepted_event("2026-04-19T10:03:00Z", "https://c.test/", "kindC", 3),
        ],
    );

    let (v, code, _, _) = env.research(&[
        "report", "s7", "--format", "rich-html", "--no-open", "--json",
    ]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["sources_count"], 3);

    let report = fs::read_to_string(env.session_dir("s7").join("report-rich.html")).unwrap();
    for url in ["a.test", "b.test", "c.test"] {
        assert!(report.contains(url), "{url} missing");
    }
}

// ── Spec acceptance #8: FORMAT_NOT_IMPLEMENTED ────────────────────────────

#[test]
fn future_format_returns_not_implemented() {
    let env = Env::new();
    env.prep_session("s8", "## Overview\nx\n");
    let (v, code, _, _) = env.research(&[
        "report", "s8", "--format", "slides-reveal", "--no-open", "--json",
    ]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "FORMAT_NOT_IMPLEMENTED");
    let supported = v["error"]["details"]["supported"].as_array().unwrap();
    assert!(supported
        .iter()
        .any(|s| s.as_str() == Some("rich-html")));
}

#[test]
fn unknown_format_returns_unsupported() {
    let env = Env::new();
    env.prep_session("s-unknown", "## Overview\nx\n");
    let (v, code, _, _) = env.research(&[
        "report", "s-unknown", "--format", "gibberish", "--no-open", "--json",
    ]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "FORMAT_UNSUPPORTED");
}

// ── Spec acceptance #9: idempotent rerun ──────────────────────────────────

#[test]
fn idempotent_rerun_overwrites() {
    let env = Env::new();
    env.prep_session("s9", "## Overview\nhello\n");
    let path = env.session_dir("s9").join("report-rich.html");

    let (v1, code1, _, _) = env.research(&[
        "report", "s9", "--format", "rich-html", "--no-open", "--json",
    ]);
    assert_eq!(code1, 0);
    let bytes1 = v1["data"]["bytes"].as_u64().unwrap();
    assert!(path.exists());

    // Change the Overview; re-run; bytes should differ deterministically.
    fs::write(
        env.session_dir("s9").join("session.md"),
        "## Overview\nsomething much longer than before to change the byte count\n",
    )
    .unwrap();
    let (v2, code2, _, _) = env.research(&[
        "report", "s9", "--format", "rich-html", "--no-open", "--json",
    ]);
    assert_eq!(code2, 0);
    let bytes2 = v2["data"]["bytes"].as_u64().unwrap();
    assert!(bytes2 != bytes1, "rerun should change output size after md change");
    assert!(path.exists(), "file still exists after rerun");
}

// ── Extra: SESSION_NOT_FOUND path ─────────────────────────────────────────

#[test]
fn session_not_found_returns_code() {
    let env = Env::new();
    let (v, code, _, _) = env.research(&[
        "report", "nope", "--format", "rich-html", "--no-open", "--json",
    ]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "SESSION_NOT_FOUND");
}

// ── brief-md (spec 2) integration tests ──────────────────────────────────

#[test]
fn brief_md_happy_path_writes_file_under_2kb() {
    let env = Env::new();
    env.prep_session(
        "bm1",
        "## Overview\nBrief overview sentence.\n\nSecond paragraph sentence.\n\n## 01 · WHY\nwhy sentence.\n\n## 02 · WHAT\nwhat sentence.\n\n## 03 · HOW\nhow sentence.\n",
    );
    env.append_jsonl(
        "bm1",
        &[
            &accepted_event("2026-04-20T10:01:00Z", "https://a.test/", "github-file", 100),
            &accepted_event("2026-04-20T10:02:00Z", "https://b.test/", "github-tree", 200),
        ],
    );
    let (v, code, stderr, _) = env.research(&[
        "report", "bm1", "--format", "brief-md", "--no-open", "--json",
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(v["data"]["format"], "brief-md");
    let out_path = v["data"]["output_path"].as_str().unwrap().to_string();
    let text = fs::read_to_string(&out_path).unwrap();
    assert!(text.len() < 2048);
    assert!(text.starts_with("# topic\n"));
    assert!(text.contains("Brief overview sentence."));
    assert!(text.contains("- **WHY**"));
    assert!(text.contains("a.test"));
    assert!(text.contains("b.test"));
    assert_eq!(v["data"]["warnings"].as_array().unwrap().len(), 0);
}

#[test]
fn brief_md_writes_default_path_when_no_output_flag() {
    let env = Env::new();
    env.prep_session("bm2", "## Overview\nsomething.\n");
    let (v, code, _, _) = env.research(&[
        "report", "bm2", "--format", "brief-md", "--no-open", "--json",
    ]);
    assert_eq!(code, 0);
    let expected = env.session_dir("bm2").join("report-brief.md");
    assert_eq!(
        v["data"]["output_path"].as_str().unwrap(),
        expected.display().to_string()
    );
    assert!(expected.exists());
}

#[test]
fn brief_md_stdout_mode_does_not_write_file() {
    let env = Env::new();
    env.prep_session("bm3", "## Overview\nstdout content.\n");
    let (_, code, _, stdout) = env.research(&[
        "report", "bm3", "--format", "brief-md", "--stdout",
    ]);
    assert_eq!(code, 0);
    assert!(stdout.contains("# topic"));
    assert!(stdout.contains("stdout content."));
    let default = env.session_dir("bm3").join("report-brief.md");
    assert!(!default.exists(), "file should not exist in --stdout mode");
}

#[test]
fn brief_md_output_flag_writes_to_specified_path() {
    let env = Env::new();
    env.prep_session("bm4", "## Overview\ncustom path.\n");
    let custom = env.session_dir("bm4").join("sub").join("b.md");
    fs::create_dir_all(custom.parent().unwrap()).unwrap();
    let (v, code, _, _) = env.research(&[
        "report",
        "bm4",
        "--format",
        "brief-md",
        "--output",
        custom.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 0);
    assert_eq!(
        v["data"]["output_path"].as_str().unwrap(),
        custom.display().to_string()
    );
    assert!(custom.exists());
    let default = env.session_dir("bm4").join("report-brief.md");
    assert!(!default.exists(), "default path should not be written when --output is set");
}

#[test]
fn brief_md_truncates_overview_over_400_chars() {
    let env = Env::new();
    let long = "x".repeat(600);
    let md = format!("## Overview\n{long}.\n");
    env.prep_session("bm5", &md);
    let (v, code, _, _) = env.research(&[
        "report", "bm5", "--format", "brief-md", "--json",
    ]);
    assert_eq!(code, 0);
    let warnings = v["data"]["warnings"].as_array().unwrap();
    assert!(warnings.iter().any(|w| w.as_str() == Some("overview_truncated")));
}

#[test]
fn brief_md_truncates_findings_over_six() {
    let env = Env::new();
    let mut md = String::from("## Overview\nshort.\n\n");
    for i in 1..=9 {
        md.push_str(&format!("## {i:02} · S{i}\nbody {i}.\n\n"));
    }
    env.prep_session("bm6", &md);
    let (v, code, _, _) = env.research(&[
        "report", "bm6", "--format", "brief-md", "--json",
    ]);
    assert_eq!(code, 0);
    let warnings = v["data"]["warnings"].as_array().unwrap();
    assert!(warnings.iter().any(|w| w.as_str() == Some("findings_truncated")));
}

#[test]
fn brief_md_missing_overview_still_fatal() {
    let env = Env::new();
    env.prep_session(
        "bm7",
        "## Overview\n<!-- placeholder -->\n\n## 01 · X\nbody.\n",
    );
    let (v, code, _, _) = env.research(&[
        "report", "bm7", "--format", "brief-md", "--json",
    ]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "MISSING_OVERVIEW");
}

#[test]
fn brief_md_ignores_diagram_references() {
    let env = Env::new();
    env.prep_session(
        "bm8",
        "## Overview\nReal.\n\n![Fig](diagrams/foo.svg)\n\n## 01 · WHY\nwhy body sentence.\n",
    );
    let (v, code, _, _) = env.research(&[
        "report", "bm8", "--format", "brief-md", "--json",
    ]);
    assert_eq!(code, 0);
    let text = fs::read_to_string(v["data"]["output_path"].as_str().unwrap()).unwrap();
    assert!(!text.contains("![Fig]"));
    assert!(!text.contains("diagrams/foo.svg"));
}

#[test]
fn brief_md_sources_from_jsonl_not_md() {
    let env = Env::new();
    env.prep_session("bm9", "## Overview\nsomething real.\n\n## 01 · WHY\nwhy.\n");
    env.append_jsonl(
        "bm9",
        &[
            &accepted_event("2026-04-20T10:00:00Z", "https://jsonl-only.test/", "k1", 1),
        ],
    );
    let (v, code, _, _) = env.research(&[
        "report", "bm9", "--format", "brief-md", "--json",
    ]);
    assert_eq!(code, 0);
    let text = fs::read_to_string(v["data"]["output_path"].as_str().unwrap()).unwrap();
    assert!(text.contains("jsonl-only.test"));
}
