//! Integration tests for research-session-series.spec.md scenarios.

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

fn research_bin() -> String {
    env!("CARGO_BIN_EXE_ascent-research").to_string()
}

struct Env {
    _tmp: TempDir,
    home: String,
    bin_dir: PathBuf,
}

impl Env {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let bin_dir = tmp.path().join("_bin");
        fs::create_dir_all(&bin_dir).unwrap();
        Self { _tmp: tmp, home, bin_dir }
    }

    fn research(&self, args: &[&str]) -> (Value, i32, String) {
        let mut cmd = Command::new(research_bin());
        cmd.args(args);
        cmd.env("ACTIONBOOK_RESEARCH_HOME", &self.home);
        cmd.env("SYNTHESIZE_NO_OPEN", "1");
        // Default to a fake json-ui that creates stub HTML so synthesize can succeed.
        cmd.env("JSON_UI_BIN", self.ensure_fake_json_ui().to_string_lossy().into_owned());
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

    fn root(&self) -> PathBuf {
        PathBuf::from(&self.home)
    }

    fn ensure_fake_json_ui(&self) -> PathBuf {
        let path = self.bin_dir.join("json-ui");
        if !path.exists() {
            fs::write(
                &path,
                r#"#!/bin/sh
out=""
while [ $# -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    shift
    out="$1"
  fi
  shift
done
[ -n "$out" ] && echo "<html><body>stub</body></html>" > "$out"
exit 0
"#,
            ).unwrap();
            let mut perms = fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&path, perms).unwrap();
        }
        path
    }
}

fn write_session_md_with_overview(path: &PathBuf) {
    fs::write(path, "\
# Research: T

## Objective
stub

## Preset
tech

## Sources
<!-- research:sources-start -->
<!-- research:sources-end -->

## Overview
This is a meaningful overview paragraph so extract_overview() treats it as real.

## Findings
### Alpha Finding
body alpha

## Notes
notes
").unwrap();
}

#[test]
fn new_from_parent_inherits_context_and_tags() {
    let env = Env::new();
    env.research(&["new", "parent topic", "--slug", "p", "--tag", "rust-series", "--json"]);
    // Parent session.md is the template — swap in a meaningful Overview.
    write_session_md_with_overview(&env.root().join("p/session.md"));

    let (v, code, stderr) = env.research(&[
        "new", "child topic", "--slug", "c", "--from", "p", "--tag", "extra", "--json",
    ]);
    assert_eq!(code, 0, "stderr: {stderr}; v={v}");
    assert_eq!(v["data"]["parent_slug"], "p");

    let tags = v["data"]["tags"].as_array().unwrap();
    let tag_strs: Vec<&str> = tags.iter().map(|t| t.as_str().unwrap()).collect();
    assert!(tag_strs.contains(&"rust-series"));
    assert!(tag_strs.contains(&"extra"));

    // Child session.md contains `## Context (from p)` block with parent overview.
    let child_md = fs::read_to_string(env.root().join("c/session.md")).unwrap();
    assert!(child_md.contains("## Context (from p)"), "child md: {child_md}");
    assert!(child_md.contains("meaningful overview paragraph"));

    // session.toml has parent_slug and tags.
    let toml = fs::read_to_string(env.root().join("c/session.toml")).unwrap();
    assert!(toml.contains(r#"parent_slug = "p""#));
    assert!(toml.contains(r#"rust-series"#));
}

#[test]
fn new_from_missing_parent_errors() {
    let env = Env::new();
    let (v, code, _) = env.research(&[
        "new", "x", "--slug", "x", "--from", "no-such-parent", "--json",
    ]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "PARENT_NOT_FOUND");
    // No session dir should have been created.
    assert!(!env.root().join("x").exists());
}

#[test]
fn list_filters_by_tag() {
    let env = Env::new();
    env.research(&["new", "a", "--slug", "a", "--tag", "x", "--json"]);
    env.research(&["new", "b", "--slug", "b", "--tag", "x", "--tag", "y", "--json"]);
    env.research(&["new", "c", "--slug", "c", "--tag", "y", "--json"]);

    let (v, code, _) = env.research(&["list", "--tag", "x", "--json"]);
    assert_eq!(code, 0);
    let slugs: Vec<&str> = v["data"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["slug"].as_str().unwrap())
        .collect();
    assert!(slugs.contains(&"a"));
    assert!(slugs.contains(&"b"));
    assert!(!slugs.contains(&"c"));
}

#[test]
fn list_tree_shows_parent_child() {
    let env = Env::new();
    env.research(&["new", "p", "--slug", "p", "--json"]);
    write_session_md_with_overview(&env.root().join("p/session.md"));
    env.research(&["new", "c1", "--slug", "c1", "--from", "p", "--json"]);
    env.research(&["new", "c2", "--slug", "c2", "--from", "p", "--json"]);
    env.research(&["new", "orph", "--slug", "orph", "--json"]);

    let (v, code, _) = env.research(&["list", "--tree", "--json"]);
    assert_eq!(code, 0);
    let tree = v["data"]["tree"].as_array().unwrap();
    let roots: Vec<&str> = tree
        .iter()
        .map(|n| n["slug"].as_str().unwrap())
        .collect();
    assert!(roots.contains(&"p"));
    assert!(roots.contains(&"orph"));
    let p_node = tree.iter().find(|n| n["slug"] == "p").unwrap();
    let children: Vec<&str> = p_node["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["slug"].as_str().unwrap())
        .collect();
    assert!(children.contains(&"c1"));
    assert!(children.contains(&"c2"));
}

#[test]
fn series_generates_index_with_multiple_members() {
    let env = Env::new();
    // 3 sessions with the same tag, all synthesized.
    for slug in ["s1", "s2", "s3"] {
        env.research(&["new", slug, "--slug", slug, "--tag", "demo", "--json"]);
        write_session_md_with_overview(&env.root().join(format!("{slug}/session.md")));
        env.research(&["synthesize", slug, "--json"]);
    }

    let (v, code, stderr) = env.research(&["series", "demo", "--json"]);
    assert_eq!(code, 0, "stderr: {stderr}; v={v}");
    assert_eq!(v["data"]["member_count"], 3);
    let index_json = env.root().join("series-demo.json");
    assert!(index_json.exists(), "index json not written");
    // HTML render uses fake json-ui (stub). Verify the json-ui document
    // itself carries all three slugs in the member list.
    let doc: Value = serde_json::from_str(&fs::read_to_string(&index_json).unwrap()).unwrap();
    let children = doc["children"].as_array().unwrap();
    let members_section = children
        .iter()
        .find(|c| {
            c["props"]["title"]
                .as_str()
                .map(|t| t.starts_with("Members"))
                .unwrap_or(false)
        })
        .expect("Members section missing");
    let items = members_section["children"][0]["props"]["items"]
        .as_array()
        .unwrap();
    assert_eq!(items.len(), 3);
    let badges: Vec<&str> = items.iter().map(|i| i["badge"].as_str().unwrap()).collect();
    for slug in ["s1", "s2", "s3"] {
        assert!(badges.contains(&slug), "missing {slug} in badges: {badges:?}");
    }
}

#[test]
fn series_warns_on_unsynthesized_member() {
    let env = Env::new();
    env.research(&["new", "done", "--slug", "done", "--tag", "s", "--json"]);
    write_session_md_with_overview(&env.root().join("done/session.md"));
    env.research(&["synthesize", "done", "--json"]);
    // second session: tagged but not synthesized
    env.research(&["new", "pending", "--slug", "pending", "--tag", "s", "--json"]);

    let (v, code, _) = env.research(&["series", "s", "--json"]);
    assert_eq!(code, 0);
    let warnings = v["data"]["warnings"].as_array().unwrap();
    let joined = warnings
        .iter()
        .map(|w| w.as_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("|");
    assert!(joined.contains("pending"), "no warning for pending: {joined}");
}

#[test]
fn series_empty_tag_errors() {
    let env = Env::new();
    env.research(&["new", "x", "--slug", "x", "--tag", "notthisone", "--json"]);
    let (v, code, _) = env.research(&["series", "different-tag", "--json"]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "TAG_NOT_FOUND");
}
