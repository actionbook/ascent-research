//! Integration tests for research-route-toml-presets.spec.md scenarios.
//!
//! Exec the release binary so we cover clap parsing + envelope shape in
//! addition to the internal matcher (which has unit coverage in rules.rs).

use serde_json::Value;
use std::process::Command;
use tempfile::TempDir;

fn binary() -> String {
    env!("CARGO_BIN_EXE_ascent-research").to_string()
}

fn run(args: &[&str]) -> (Value, i32, String) {
    let out = Command::new(binary())
        .args(args)
        .output()
        .expect("spawn research");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let json_line = stdout.lines().find(|l| l.trim_start().starts_with('{'));
    let v: Value = match json_line {
        Some(l) => serde_json::from_str(l).unwrap_or(Value::Null),
        None => Value::Null,
    };
    (v, out.status.code().unwrap_or(-1), stderr)
}

#[test]
fn route_tech_hn_item() {
    let (v, code, _) = run(&[
        "route",
        "https://news.ycombinator.com/item?id=12345",
        "--json",
    ]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["executor"], "postagent");
    assert_eq!(v["data"]["kind"], "hn-item");
    let cmd = v["data"]["command_template"].as_str().unwrap();
    assert!(
        cmd.contains("/v0/item/12345.json"),
        "missing HN API URL: {cmd}"
    );
}

#[test]
fn route_tech_hn_topstories() {
    for url in [
        "https://news.ycombinator.com/",
        "https://news.ycombinator.com/news",
    ] {
        let (v, code, _) = run(&["route", url, "--json"]);
        assert_eq!(code, 0, "for {url}");
        assert_eq!(v["data"]["kind"], "hn-topstories", "for {url}");
    }
}

#[test]
fn route_tech_github_repo() {
    let (v, code, _) = run(&[
        "route",
        "https://github.com/bytedance/monoio",
        "--json",
    ]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["executor"], "postagent");
    assert_eq!(v["data"]["kind"], "github-repo-readme");
    let cmd = v["data"]["command_template"].as_str().unwrap();
    assert!(cmd.contains("/repos/bytedance/monoio/readme"), "got {cmd}");
}

#[test]
fn route_tech_github_issue() {
    let (v, code, _) = run(&[
        "route",
        "https://github.com/tokio-rs/tokio/issues/8056",
        "--json",
    ]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["kind"], "github-issue");
}

#[test]
fn route_tech_arxiv_abs() {
    let (v, code, _) = run(&["route", "https://arxiv.org/abs/2601.12345", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["kind"], "arxiv-abs");
    let cmd = v["data"]["command_template"].as_str().unwrap();
    assert!(cmd.contains("id_list=2601.12345"), "got {cmd}");
}

#[test]
fn route_tech_github_file_blob_to_raw() {
    let (v, code, _) = run(&[
        "route",
        "https://github.com/tokio-rs/tokio/blob/master/tokio/src/runtime/mod.rs",
        "--json",
    ]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["executor"], "postagent");
    assert_eq!(v["data"]["kind"], "github-file");
    let cmd = v["data"]["command_template"].as_str().unwrap();
    assert!(
        cmd.contains("raw.githubusercontent.com/tokio-rs/tokio/master/tokio/src/runtime/mod.rs"),
        "got {cmd}"
    );
}

#[test]
fn route_tech_github_tree_to_contents_api() {
    let (v, code, _) = run(&[
        "route",
        "https://github.com/tokio-rs/tokio/tree/master/tokio/src/runtime",
        "--json",
    ]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["kind"], "github-tree");
    let cmd = v["data"]["command_template"].as_str().unwrap();
    assert!(
        cmd.contains("api.github.com/repos/tokio-rs/tokio/contents/tokio/src/runtime?ref=master"),
        "got {cmd}"
    );
}

#[test]
fn route_tech_github_raw_passthrough() {
    let (v, code, _) = run(&[
        "route",
        "https://raw.githubusercontent.com/rust-lang/rust/master/README.md",
        "--json",
    ]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["kind"], "github-raw");
    let cmd = v["data"]["command_template"].as_str().unwrap();
    assert!(
        cmd.contains("raw.githubusercontent.com/rust-lang/rust/master/README.md"),
        "got {cmd}"
    );
}

#[test]
fn route_fallback_unknown_domain() {
    let (v, code, _) = run(&[
        "route",
        "https://corrode.dev/blog/async/",
        "--json",
    ]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["executor"], "browser");
    assert_eq!(v["data"]["kind"], "browser-fallback");
    assert_eq!(v["data"]["classification"], "fallback");
}

#[test]
fn route_prefer_browser_forces() {
    let (v, code, _) = run(&[
        "route",
        "https://github.com/foo/bar",
        "--prefer",
        "browser",
        "--json",
    ]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["kind"], "browser-forced");
    assert_eq!(v["data"]["classification"], "forced");
}

#[test]
fn route_invalid_url_errors() {
    let (v, code, _) = run(&["route", "not-a-url", "--json"]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "INVALID_ARGUMENT");
}

#[test]
fn route_preset_file_not_found() {
    let (v, code, _) = run(&[
        "route",
        "https://example.com/",
        "--rules",
        "/no/such/path.toml",
        "--json",
    ]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "PRESET_ERROR");
    assert_eq!(v["error"]["details"]["sub_code"], "FILE_NOT_FOUND");
}

#[test]
fn route_custom_rules_file_overrides_builtin() {
    let tmp = TempDir::new().unwrap();
    let custom = tmp.path().join("custom.toml");
    std::fs::write(
        &custom,
        r#"
name = "custom"
[[rule]]
kind = "ex"
host = "example.com"
path = "/foo"
executor = "postagent"
template = 'postagent send --anonymous "example/{path}"'
[fallback]
kind = "fb"
executor = "browser"
template = "fb"
"#,
    )
    .unwrap();
    let (v, code, _) = run(&[
        "route",
        "https://example.com/foo",
        "--rules",
        custom.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["executor"], "postagent");
    assert_eq!(v["data"]["kind"], "ex");
    assert_eq!(v["data"]["preset"], "custom");
}

#[test]
fn route_placeholder_unbound_error() {
    let tmp = TempDir::new().unwrap();
    let custom = tmp.path().join("bad.toml");
    std::fs::write(
        &custom,
        r#"
name = "bad"
[[rule]]
kind = "x"
host = "example.com"
path = "/x"
executor = "postagent"
template = "echo {foo}"
[fallback]
kind = "fb"
executor = "browser"
template = "fb"
"#,
    )
    .unwrap();
    let (v, code, _) = run(&[
        "route",
        "https://example.com/x",
        "--rules",
        custom.to_str().unwrap(),
        "--json",
    ]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "PRESET_ERROR");
    assert_eq!(v["error"]["details"]["sub_code"], "PLACEHOLDER_UNBOUND");
}
