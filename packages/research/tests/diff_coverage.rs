//! Integration tests for `research diff` and `research coverage`
//! (spec: research-autoresearcher-helpers).

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
        cmd.env("RESEARCH_NO_OPEN", "1");
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

    fn prep_session(&self, slug: &str, body: &str) {
        let (_, code, stderr) = self.research(&["new", "topic", "--slug", slug, "--json"]);
        assert_eq!(code, 0, "new failed: {stderr}");
        fs::write(self.session_dir(slug).join("session.md"), body).unwrap();
    }

    fn prep_fact_check_session(&self, slug: &str, body: &str) {
        let (_, code, stderr) = self.research(&[
            "new",
            "topic",
            "--slug",
            slug,
            "--tag",
            "fact-check",
            "--json",
        ]);
        assert_eq!(code, 0, "new failed: {stderr}");
        fs::write(self.session_dir(slug).join("session.md"), body).unwrap();
    }

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

fn digested_event(ts: &str, url: &str) -> String {
    format!(
        r###"{{"event":"source_digested","timestamp":"{ts}","iteration":1,"url":"{url}","into_section":"## Overview"}}"###
    )
}

fn fact_checked_event(ts: &str, url: &str, outcome: &str) -> String {
    format!(
        r###"{{"event":"fact_checked","timestamp":"{ts}","iteration":1,"claim":"claim","query":"query","sources":["{url}"],"outcome":"{outcome}","into_section":"## Overview"}}"###
    )
}

fn report_ready_md(url: &str) -> String {
    let overview: String = "z".repeat(300);
    format!(
        "## Overview\n{overview}\n\n## 01 · A\nbody a.\n\n## 02 · B\nbody b.\n\n## 03 · C\nbody c.\n\n![f](diagrams/g.svg)\n\nSee [x]({url}).\n"
    )
}

fn resolve_report_ready_diagram(env: &Env, slug: &str) {
    let diag = env.session_dir(slug).join("diagrams");
    fs::create_dir_all(&diag).unwrap();
    fs::write(diag.join("g.svg"), "<svg/>").unwrap();
}

// ── diff tests ────────────────────────────────────────────────────────────

#[test]
fn diff_finds_unused_accepted_source() {
    let env = Env::new();
    env.prep_session(
        "d1",
        "## Overview\nSome real content paragraph talking about things.\n",
    );
    env.append_jsonl(
        "d1",
        &[&accepted_event(
            "2026-04-20T10:00:00Z",
            "https://fetched-but-uncited.test/",
            "k",
            1,
        )],
    );
    let (v, code, _) = env.research(&["diff", "d1", "--json"]);
    assert_eq!(code, 0);
    let unused = v["data"]["unused_sources"].as_array().unwrap();
    assert!(
        unused
            .iter()
            .any(|u| u.as_str() == Some("https://fetched-but-uncited.test/"))
    );
}

#[test]
fn diff_finds_hallucinated_md_link() {
    let env = Env::new();
    env.prep_session(
        "d2",
        "## Overview\nCheck out [foo](https://hallucinated.test/) for details.\n",
    );
    // No jsonl acceptance → the URL is hallucinated.
    let (v, code, _) = env.research(&["diff", "d2", "--json"]);
    assert_eq!(code, 0);
    let missing = v["data"]["missing_sources"].as_array().unwrap();
    assert!(
        missing
            .iter()
            .any(|u| u.as_str() == Some("https://hallucinated.test/"))
    );
}

#[test]
fn diff_unused_only_flag_excludes_missing() {
    let env = Env::new();
    env.prep_session("d3", "## Overview\nBody cites [x](https://hall.test/).\n");
    env.append_jsonl(
        "d3",
        &[&accepted_event(
            "2026-04-20T10:00:00Z",
            "https://unused.test/",
            "k",
            1,
        )],
    );
    let (v, code, _) = env.research(&["diff", "d3", "--unused-only", "--json"]);
    assert_eq!(code, 0);
    assert!(
        v["data"]["unused_sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u.as_str() == Some("https://unused.test/"))
    );
    assert!(
        v["data"].get("missing_sources").is_none(),
        "--unused-only suppresses missing set"
    );
}

#[test]
fn diff_clean_session_both_arrays_empty() {
    let env = Env::new();
    env.prep_session(
        "d4",
        "## Overview\nAll cited. See [a](https://a.test/) and [b](https://b.test/).\n",
    );
    env.append_jsonl(
        "d4",
        &[
            &accepted_event("2026-04-20T10:00:00Z", "https://a.test/", "k", 1),
            &accepted_event("2026-04-20T10:01:00Z", "https://b.test/", "k", 1),
        ],
    );
    let (v, code, _) = env.research(&["diff", "d4", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["unused_sources"].as_array().unwrap().len(), 0);
    assert_eq!(v["data"]["missing_sources"].as_array().unwrap().len(), 0);
}

#[test]
fn diff_excludes_sources_block_from_body_scan() {
    let env = Env::new();
    // A URL only inside the sources block (via MD link form inside the
    // block) must NOT count as "body citation".
    env.prep_session(
        "d5",
        "## Overview\nsomething real\n\n## Sources\n<!-- research:sources-start -->\n- [k](https://cache.test/)\n<!-- research:sources-end -->\n",
    );
    env.append_jsonl(
        "d5",
        &[&accepted_event(
            "2026-04-20T10:00:00Z",
            "https://cache.test/",
            "k",
            1,
        )],
    );
    let (v, code, _) = env.research(&["diff", "d5", "--json"]);
    assert_eq!(code, 0);
    // Accepted but not cited outside the sources block → unused
    let unused = v["data"]["unused_sources"].as_array().unwrap();
    assert!(
        unused
            .iter()
            .any(|u| u.as_str() == Some("https://cache.test/"))
    );
}

// ── coverage tests ───────────────────────────────────────────────────────

#[test]
fn coverage_basic_counts() {
    let env = Env::new();
    let overview: String = "x".repeat(300);
    let md = format!(
        "## Overview\n{overview}\n\n## 01 · WHY\na body\n\n## 02 · WHAT\nanother body\n\n## 03 · HOW\n3rd\n\n> **aside:** thesis here\n\n![f](diagrams/a.svg)\n"
    );
    env.prep_session("c1", &md);
    // Resolve the diagram.
    let diag = env.session_dir("c1").join("diagrams");
    fs::create_dir_all(&diag).unwrap();
    fs::write(diag.join("a.svg"), "<svg/>").unwrap();
    env.append_jsonl(
        "c1",
        &[&accepted_event(
            "2026-04-20T10:00:00Z",
            "https://x.test/",
            "k",
            1,
        )],
    );

    let (v, code, _) = env.research(&["coverage", "c1", "--json"]);
    assert_eq!(code, 0);
    let d = &v["data"];
    assert_eq!(d["numbered_sections_count"], 3);
    assert_eq!(d["aside_count"], 1);
    assert_eq!(d["diagrams_referenced"], 1);
    assert_eq!(d["diagrams_resolved"], 1);
    assert_eq!(d["sources_accepted"], 1);
    assert!(d["overview_chars"].as_u64().unwrap() >= 300);
}

#[test]
fn coverage_report_ready_all_green() {
    let env = Env::new();
    let overview: String = "y".repeat(300);
    let md = format!(
        "## Overview\n{overview}\n\n## 01 · A\nbody a\n\n## 02 · B\nbody b\n\n## 03 · C\nbody c\n\n![f](diagrams/g.svg)\n\nSee [x](https://ok.test/).\n"
    );
    env.prep_session("c2", &md);
    let diag = env.session_dir("c2").join("diagrams");
    fs::create_dir_all(&diag).unwrap();
    fs::write(diag.join("g.svg"), "<svg/>").unwrap();
    env.append_jsonl(
        "c2",
        &[&accepted_event(
            "2026-04-20T10:00:00Z",
            "https://ok.test/",
            "k",
            1,
        )],
    );

    let (v, code, _) = env.research(&["coverage", "c2", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["report_ready"], true);
    assert_eq!(
        v["data"]["report_ready_blockers"].as_array().unwrap().len(),
        0
    );
}

#[test]
fn coverage_fact_check_tag_blocks_without_fact_checked_event() {
    let env = Env::new();
    let url = "https://official.test/roster";
    env.prep_fact_check_session("fcov1", &report_ready_md(url));
    resolve_report_ready_diagram(&env, "fcov1");
    let accepted = accepted_event("2026-04-20T10:00:00Z", url, "official", 1);
    env.append_jsonl("fcov1", &[&accepted]);

    let (v, code, _) = env.research(&["coverage", "fcov1", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["fact_check_required"], true);
    assert_eq!(v["data"]["report_ready"], false);
    let blockers = v["data"]["report_ready_blockers"].as_array().unwrap();
    assert!(
        blockers
            .iter()
            .any(|b| b.as_str().unwrap().contains("fact_checks_total 0 < 1")),
        "expected fact-check blocker; got: {blockers:?}"
    );
}

#[test]
fn coverage_fact_check_tag_ready_with_fact_checked_event() {
    let env = Env::new();
    let url = "https://official.test/roster";
    env.prep_fact_check_session("fcov2", &report_ready_md(url));
    resolve_report_ready_diagram(&env, "fcov2");
    let accepted = accepted_event("2026-04-20T10:00:00Z", url, "official", 1);
    let digested = digested_event("2026-04-20T10:00:30Z", url);
    let checked = fact_checked_event("2026-04-20T10:01:00Z", url, "supported");
    env.append_jsonl("fcov2", &[&accepted, &digested, &checked]);

    let (v, code, _) = env.research(&["coverage", "fcov2", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["fact_check_required"], true);
    assert_eq!(v["data"]["fact_checks_total"], 1);
    assert_eq!(v["data"]["fact_checks_supported"], 1);
    assert_eq!(v["data"]["fact_checks_refuted"], 0);
    assert_eq!(v["data"]["fact_checks_uncertain"], 0);
    assert_eq!(v["data"]["fact_check_invalid_sources"], 0);
    assert_eq!(v["data"]["fact_check_undigested_sources"], 0);
    let blockers = v["data"]["report_ready_blockers"].as_array().unwrap();
    assert!(
        !blockers
            .iter()
            .any(|b| b.as_str().unwrap().contains("fact_check")),
        "unexpected fact-check blocker: {blockers:?}"
    );
}

#[test]
fn coverage_fact_check_invalid_sources_blocks_report_ready() {
    let env = Env::new();
    let cited_url = "https://official.test/roster";
    let missing_url = "https://missing-source.test/";
    env.prep_fact_check_session("fcov3", &report_ready_md(cited_url));
    resolve_report_ready_diagram(&env, "fcov3");
    let accepted = accepted_event("2026-04-20T10:00:00Z", cited_url, "official", 1);
    let checked = fact_checked_event("2026-04-20T10:01:00Z", missing_url, "uncertain");
    env.append_jsonl("fcov3", &[&accepted, &checked]);

    let (v, code, _) = env.research(&["coverage", "fcov3", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["fact_check_invalid_sources"], 1);
    assert_eq!(v["data"]["report_ready"], false);
    let blockers = v["data"]["report_ready_blockers"].as_array().unwrap();
    assert!(
        blockers.iter().any(|b| {
            b.as_str()
                .unwrap()
                .contains("fact_check_invalid_sources 1 > 0")
        }),
        "expected invalid-source blocker; got: {blockers:?}"
    );
}

#[test]
fn coverage_fact_check_refuted_blocks_report_ready() {
    let env = Env::new();
    let url = "https://official.test/roster";
    env.prep_fact_check_session("fcov4", &report_ready_md(url));
    resolve_report_ready_diagram(&env, "fcov4");
    let accepted = accepted_event("2026-04-20T10:00:00Z", url, "official", 1);
    let digested = digested_event("2026-04-20T10:00:30Z", url);
    let checked = fact_checked_event("2026-04-20T10:01:00Z", url, "refuted");
    env.append_jsonl("fcov4", &[&accepted, &digested, &checked]);

    let (v, code, _) = env.research(&["coverage", "fcov4", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["fact_checks_refuted"], 1);
    assert_eq!(v["data"]["report_ready"], false);
    let blockers = v["data"]["report_ready_blockers"].as_array().unwrap();
    assert!(
        blockers
            .iter()
            .any(|b| b.as_str().unwrap().contains("fact_checks_refuted 1 > 0")),
        "expected refuted blocker; got: {blockers:?}"
    );
}

#[test]
fn coverage_fact_check_uncertain_blocks_report_ready() {
    let env = Env::new();
    let url = "https://official.test/roster";
    env.prep_fact_check_session("fcov5", &report_ready_md(url));
    resolve_report_ready_diagram(&env, "fcov5");
    let accepted = accepted_event("2026-04-20T10:00:00Z", url, "official", 1);
    let digested = digested_event("2026-04-20T10:00:30Z", url);
    let checked = fact_checked_event("2026-04-20T10:01:00Z", url, "uncertain");
    env.append_jsonl("fcov5", &[&accepted, &digested, &checked]);

    let (v, code, _) = env.research(&["coverage", "fcov5", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["fact_checks_uncertain"], 1);
    assert_eq!(v["data"]["report_ready"], false);
    let blockers = v["data"]["report_ready_blockers"].as_array().unwrap();
    assert!(
        blockers
            .iter()
            .any(|b| b.as_str().unwrap().contains("fact_checks_uncertain 1 > 0")),
        "expected uncertain blocker; got: {blockers:?}"
    );
}

#[test]
fn coverage_fact_check_undigested_sources_blocks_report_ready() {
    let env = Env::new();
    let url = "https://official.test/roster";
    env.prep_fact_check_session("fcov6", &report_ready_md(url));
    resolve_report_ready_diagram(&env, "fcov6");
    let accepted = accepted_event("2026-04-20T10:00:00Z", url, "official", 1);
    let checked = fact_checked_event("2026-04-20T10:01:00Z", url, "supported");
    env.append_jsonl("fcov6", &[&accepted, &checked]);

    let (v, code, _) = env.research(&["coverage", "fcov6", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["fact_check_undigested_sources"], 1);
    assert_eq!(v["data"]["report_ready"], false);
    let blockers = v["data"]["report_ready_blockers"].as_array().unwrap();
    assert!(
        blockers.iter().any(|b| {
            b.as_str()
                .unwrap()
                .contains("fact_check_undigested_sources 1 > 0")
        }),
        "expected undigested-source blocker; got: {blockers:?}"
    );
}

#[test]
fn coverage_short_overview_blocks() {
    let env = Env::new();
    env.prep_session("c3", "## Overview\nshort.\n");
    let (v, code, _) = env.research(&["coverage", "c3", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["report_ready"], false);
    let blockers = v["data"]["report_ready_blockers"].as_array().unwrap();
    assert!(
        blockers
            .iter()
            .any(|b| b.as_str().unwrap().contains("overview_chars"))
    );
}

#[test]
fn coverage_no_diagram_blocks() {
    let env = Env::new();
    let overview: String = "y".repeat(300);
    let md = format!(
        "## Overview\n{overview}\n\n## 01 · A\nbody a.\n\n## 02 · B\nbody b.\n\n## 03 · C\nbody c.\n"
    );
    env.prep_session("c4", &md);
    env.append_jsonl(
        "c4",
        &[&accepted_event(
            "2026-04-20T10:00:00Z",
            "https://ok.test/",
            "k",
            1,
        )],
    );
    let (v, code, _) = env.research(&["coverage", "c4", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["report_ready"], false);
    let blockers = v["data"]["report_ready_blockers"].as_array().unwrap();
    assert!(
        blockers
            .iter()
            .any(|b| b.as_str().unwrap().contains("diagrams_referenced"))
    );
}

#[test]
fn coverage_hallucinated_source_blocks() {
    let env = Env::new();
    let overview: String = "y".repeat(300);
    let md = format!(
        "## Overview\n{overview}\n\n## 01 · A\nbody [hallucinated](https://hall.test/) text.\n\n## 02 · B\nb.\n\n## 03 · C\nc.\n\n![f](diagrams/g.svg)\n"
    );
    env.prep_session("c5", &md);
    let diag = env.session_dir("c5").join("diagrams");
    fs::create_dir_all(&diag).unwrap();
    fs::write(diag.join("g.svg"), "<svg/>").unwrap();
    env.append_jsonl(
        "c5",
        &[&accepted_event(
            "2026-04-20T10:00:00Z",
            "https://ok.test/",
            "k",
            1,
        )],
    );
    let (v, code, _) = env.research(&["coverage", "c5", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["report_ready"], false);
    let blockers = v["data"]["report_ready_blockers"].as_array().unwrap();
    assert!(
        blockers
            .iter()
            .any(|b| b.as_str().unwrap().contains("sources_hallucinated"))
    );
}

// v2 Step 2 — source_kind_diversity (output-only, non-blocker) ───────────

#[test]
fn coverage_reports_source_kind_diversity() {
    let env = Env::new();
    let overview: String = "y".repeat(300);
    let md =
        format!("## Overview\n{overview}\n\n## 01 · A\na.\n\n## 02 · B\nb.\n\n## 03 · C\nc.\n");
    env.prep_session("cdiv", &md);
    env.append_jsonl(
        "cdiv",
        &[
            &accepted_event("2026-04-20T10:00:00Z", "https://a.test/", "arxiv-abs", 1),
            &accepted_event("2026-04-20T10:00:01Z", "https://b.test/", "github-repo", 1),
            &accepted_event("2026-04-20T10:00:02Z", "https://c.test/", "hn-item", 1),
            // Duplicate kind — must not inflate the diversity count.
            &accepted_event("2026-04-20T10:00:03Z", "https://d.test/", "arxiv-abs", 1),
        ],
    );
    let (v, code, _) = env.research(&["coverage", "cdiv", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(
        v["data"]["source_kind_diversity"], 3,
        "3 unique kinds from 4 accepted sources"
    );
    assert_eq!(v["data"]["sources_accepted"], 4);
}
