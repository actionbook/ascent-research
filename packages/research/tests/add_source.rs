//! Integration tests for research-add-source.spec.md using fake subprocess
//! binaries injected via POSTAGENT_BIN / ACTIONBOOK_BIN env vars.
//!
//! Each test writes a small shell script that produces a scripted JSON
//! response, then runs `research add <url>` and asserts on the envelope.

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

fn research_bin() -> String {
    env!("CARGO_BIN_EXE_ascent-research").to_string()
}

/// Per-test isolated home + fake-binary factory.
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

    fn write_fake_bin(&self, name: &str, script: &str) -> PathBuf {
        let path = self.bin_dir.join(name);
        fs::write(&path, script).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn research(&self, args: &[&str]) -> (Value, i32, String) {
        let mut cmd = Command::new(research_bin());
        cmd.args(args);
        cmd.env("ACTIONBOOK_RESEARCH_HOME", &self.home);
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

    fn research_with_bins(
        &self,
        args: &[&str],
        postagent: Option<&str>,
        actionbook: Option<&str>,
    ) -> (Value, i32, String) {
        let mut cmd = Command::new(research_bin());
        cmd.args(args);
        cmd.env("ACTIONBOOK_RESEARCH_HOME", &self.home);
        if let Some(p) = postagent {
            cmd.env("POSTAGENT_BIN", p);
        }
        if let Some(a) = actionbook {
            cmd.env("ACTIONBOOK_BIN", a);
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

// ── Fake subprocess scripts ─────────────────────────────────────────────────

fn fake_postagent_happy() -> String {
    // Real postagent contract: raw response body on stdout, stderr empty.
    r#"#!/bin/sh
cat <<'JSON'
{"title":"Hello","score":100}
JSON
"#.to_string()
}

fn fake_postagent_404() -> String {
    // Real postagent contract on 4xx: stdout empty, stderr carries the
    // ⚠ <status> — ... warning line (+ optional body echo).
    r#"#!/bin/sh
printf '%s\n' '⚠ 404 — endpoint does not exist at https://example.test/missing' >&2
printf '%s\n' 'HTTP 404 Not Found' >&2
printf '%s\n' '{"message":"Not Found"}' >&2
exit 0
"#.to_string()
}

fn fake_postagent_exit_1() -> String {
    r#"#!/bin/sh
echo "simulated failure" >&2
exit 1
"#.to_string()
}

fn fake_actionbook_happy(observed_url: &str, body_len: usize) -> String {
    let body: String = "x".repeat(body_len);
    // actionbook subcommand dispatcher — we branch on $1/$2 to emit appropriate
    // envelopes. Only `browser text` needs a real data.value payload.
    format!(
        r#"#!/bin/sh
# Fake actionbook — understands: start, new-tab, wait, text, close-tab
sub="$1"
case "$sub" in
  browser)
    case "$2" in
      start)
        printf '%s\n' '{{"ok":true,"command":"browser start","context":{{}},"data":{{}},"error":null,"meta":{{"duration_ms":0,"warnings":[]}}}}'
        exit 0 ;;
      new-tab)
        printf '%s\n' '{{"ok":true,"command":"browser new-tab","context":{{"url":"{obs}"}},"data":{{}},"error":null,"meta":{{"duration_ms":0,"warnings":[]}}}}'
        exit 0 ;;
      wait)
        printf '%s\n' '{{"ok":true,"command":"browser wait network-idle","context":{{}},"data":{{}},"error":null,"meta":{{"duration_ms":0,"warnings":[]}}}}'
        exit 0 ;;
      text)
        printf '%s\n' '{{"ok":true,"command":"browser text","context":{{"url":"{obs}"}},"data":{{"value":"{body}"}},"error":null,"meta":{{"duration_ms":0,"warnings":[]}}}}'
        exit 0 ;;
      close-tab)
        printf '%s\n' '{{"ok":true,"command":"browser close-tab","context":{{}},"data":{{}},"error":null,"meta":{{"duration_ms":0,"warnings":[]}}}}'
        exit 0 ;;
    esac
    ;;
esac
exit 0
"#,
        obs = observed_url,
        body = body
    )
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[test]
fn add_postagent_happy_path_envelope() {
    let env = Env::new();
    env.research(&["new", "t1", "--slug", "t1", "--json"]);
    let pa = env.write_fake_bin("postagent", &fake_postagent_happy());

    let (v, code, stderr) = env.research_with_bins(
        &[
            "add",
            "https://news.ycombinator.com/item?id=123",
            "--slug",
            "t1",
            "--json",
        ],
        Some(pa.to_str().unwrap()),
        None,
    );
    assert_eq!(code, 0, "stderr: {stderr}; envelope: {v}");
    // Five independent observability fields present:
    assert_eq!(v["data"]["route_decision"]["executor"], "postagent");
    assert_eq!(v["data"]["route_decision"]["kind"], "hn-item");
    assert_eq!(v["data"]["fetch_success"], true);
    assert_eq!(v["data"]["smell_pass"], true);
    assert!(v["data"]["bytes"].as_u64().unwrap() > 0);
    assert!(v["data"]["warnings"].is_array());
    assert_eq!(v["data"]["trust_score"], 2.0);

    // raw/ file exists
    let raw_path = env.session_dir("t1").join(
        v["data"]["raw_path"]
            .as_str()
            .unwrap()
            .trim_start_matches("raw/"),
    );
    let raw_candidate_1 = env.session_dir("t1").join(v["data"]["raw_path"].as_str().unwrap());
    assert!(
        raw_path.exists() || raw_candidate_1.exists(),
        "raw file missing at either {:?} or {:?}",
        raw_path,
        raw_candidate_1
    );

    // session.jsonl has source_attempted + source_accepted
    let jsonl = fs::read_to_string(env.session_dir("t1").join("session.jsonl")).unwrap();
    assert!(jsonl.contains("source_attempted"));
    assert!(jsonl.contains("source_accepted"));

    // session.md Sources block mentions the URL
    let md = fs::read_to_string(env.session_dir("t1").join("session.md")).unwrap();
    assert!(md.contains("https://news.ycombinator.com/item?id=123"), "md: {md}");
}

#[test]
fn add_api_error_rejects_with_observability_envelope() {
    let env = Env::new();
    env.research(&["new", "t1", "--slug", "t1", "--json"]);
    let pa = env.write_fake_bin("postagent", &fake_postagent_404());

    let (v, code, _) = env.research_with_bins(
        &[
            "add",
            "https://github.com/foo/bar",
            "--slug",
            "t1",
            "--json",
        ],
        Some(pa.to_str().unwrap()),
        None,
    );
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "SMELL_REJECTED");
    let d = &v["error"]["details"];
    assert_eq!(d["reject_reason"], "api_error");
    assert_eq!(d["fetch_success"], true); // subprocess exited 0, but HTTP was 4xx
    assert_eq!(d["smell_pass"], false);
    assert!(d["warnings"].is_array());
    // rejected raw file exists
    let rejected = d["rejected_raw_path"].as_str().unwrap();
    let full = env.session_dir("t1").join(rejected.trim_start_matches("raw/"));
    assert!(
        env.session_dir("t1").join(rejected).exists() || full.exists(),
        "rejected raw file missing: {rejected}"
    );
}

#[test]
fn add_subprocess_fetch_failed() {
    let env = Env::new();
    env.research(&["new", "t1", "--slug", "t1", "--json"]);
    let pa = env.write_fake_bin("postagent", &fake_postagent_exit_1());

    let (v, code, _) = env.research_with_bins(
        &[
            "add",
            "https://news.ycombinator.com/item?id=99",
            "--slug",
            "t1",
            "--json",
        ],
        Some(pa.to_str().unwrap()),
        None,
    );
    assert_ne!(code, 0);
    assert_eq!(v["error"]["details"]["reject_reason"], "fetch_failed");
    assert_eq!(v["error"]["details"]["fetch_success"], false);
}

#[test]
fn add_browser_wrong_url_rejects() {
    let env = Env::new();
    env.research(&["new", "t1", "--slug", "t1", "--json"]);
    // Fake actionbook that claims the tab is at about:blank
    let ab = env.write_fake_bin(
        "actionbook",
        &fake_actionbook_happy("about:blank", 0),
    );

    let (v, code, _) = env.research_with_bins(
        &[
            "add",
            "https://corrode.dev/blog/async/",
            "--slug",
            "t1",
            "--json",
        ],
        None,
        Some(ab.to_str().unwrap()),
    );
    assert_ne!(code, 0);
    assert_eq!(v["error"]["details"]["reject_reason"], "wrong_url");
    assert_eq!(v["error"]["details"]["observed_url"], "about:blank");
    assert_eq!(v["error"]["details"]["smell_pass"], false);
}

#[test]
fn add_browser_happy_article() {
    let env = Env::new();
    env.research(&["new", "t1", "--slug", "t1", "--json"]);
    let url = "https://corrode.dev/blog/async/";
    let ab = env.write_fake_bin("actionbook", &fake_actionbook_happy(url, 3000));

    let (v, code, stderr) = env.research_with_bins(
        &["add", url, "--slug", "t1", "--json"],
        None,
        Some(ab.to_str().unwrap()),
    );
    assert_eq!(code, 0, "stderr: {stderr}; v={v}");
    assert_eq!(v["data"]["route_decision"]["executor"], "browser");
    assert_eq!(v["data"]["smell_pass"], true);
    // 3000-byte body + readable heuristic (blog path) → trust 1.5
    assert_eq!(v["data"]["trust_score"], 1.5);
}

#[test]
fn add_duplicate_url_rejects() {
    let env = Env::new();
    env.research(&["new", "t1", "--slug", "t1", "--json"]);
    let pa = env.write_fake_bin("postagent", &fake_postagent_happy());

    // First time accepts
    let (_, code1, _) = env.research_with_bins(
        &[
            "add",
            "https://news.ycombinator.com/item?id=1",
            "--slug",
            "t1",
            "--json",
        ],
        Some(pa.to_str().unwrap()),
        None,
    );
    assert_eq!(code1, 0);

    // Second time rejects as duplicate
    let (v, code2, _) = env.research_with_bins(
        &[
            "add",
            "https://news.ycombinator.com/item?id=1",
            "--slug",
            "t1",
            "--json",
        ],
        Some(pa.to_str().unwrap()),
        None,
    );
    assert_ne!(code2, 0);
    assert_eq!(v["error"]["details"]["reject_reason"], "duplicate");
}

#[test]
fn add_missing_dependency_when_binary_not_found() {
    let env = Env::new();
    env.research(&["new", "t1", "--slug", "t1", "--json"]);

    let (v, code, _) = env.research_with_bins(
        &[
            "add",
            "https://news.ycombinator.com/item?id=1",
            "--slug",
            "t1",
            "--json",
        ],
        Some("/definitely/no/such/binary/postagent"),
        None,
    );
    assert_ne!(code, 0);
    // warning contains MISSING_DEPENDENCY marker
    let warnings = v["error"]["details"]["warnings"].as_array().unwrap();
    let has_missing = warnings
        .iter()
        .any(|w| w.as_str().unwrap_or("").contains("MISSING_DEPENDENCY"));
    assert!(has_missing, "expected MISSING_DEPENDENCY warning; got {warnings:?}");
}

#[test]
fn argv_boundary_does_not_invoke_shell() {
    // Deliberately malformed byte string containing shell metacharacters
    // (spaces, quotes, semicolons). This is NOT a valid URL per RFC 3986 —
    // the test is about what happens if someone (or a misbehaving LLM)
    // passes such bytes anyway. Rust's `Command::arg` does not invoke a
    // shell, so `touch /tmp/pwned_research` must NOT execute.
    let env = Env::new();
    env.research(&["new", "t1", "--slug", "t1", "--json"]);

    let argv_log = env.bin_dir.join("argv.log");
    let argv_log_str = argv_log.to_string_lossy().into_owned();
    let script = format!(
        r#"#!/bin/sh
for arg in "$@"; do
  printf '%s\n' "$arg" >> "{log}"
done
cat <<'JSON'
{{"status":200,"body":{{"ok":true}},"headers":{{}}}}
JSON
"#,
        log = argv_log_str
    );
    let pa = env.write_fake_bin("postagent", &script);

    let evil = r#"https://news.ycombinator.com/item?id=1"; touch /tmp/pwned_research; echo ""#;
    let (_, _, _) = env.research_with_bins(
        &["add", evil, "--slug", "t1", "--json"],
        Some(pa.to_str().unwrap()),
        None,
    );

    assert!(
        !std::path::Path::new("/tmp/pwned_research").exists(),
        "shell injection bypassed argv boundary"
    );
    if argv_log.exists() {
        let argv_text = fs::read_to_string(&argv_log).unwrap_or_default();
        assert!(argv_text.lines().count() > 0);
    }
}

#[test]
fn percent_encoded_url_with_suspicious_chars_routes_cleanly() {
    // Counterpart to argv_boundary_does_not_invoke_shell: when the URL is
    // a *valid* (properly percent-encoded) string that happens to contain
    // shell-looking bytes in its decoded form, the CLI should still route
    // it without any shell expansion. With a non-matching host, it falls
    // through to browser-fallback; the matcher must treat the string as
    // opaque bytes.
    let env = Env::new();
    env.research(&["new", "t1", "--slug", "t1", "--json"]);
    let ab = env.write_fake_bin(
        "actionbook",
        &fake_actionbook_happy("https://example.test/x", 800),
    );

    // %22 = " , %20 = space , %3B = ;
    let encoded =
        "https://example.test/x?arg=1%22%3B%20touch%20/tmp/pwned_encoded%3B%20echo%20%22";
    let (v, code, _) = env.research_with_bins(
        &["add", encoded, "--slug", "t1", "--json"],
        None,
        Some(ab.to_str().unwrap()),
    );
    // Router doesn't recognize example.test so it falls back to browser.
    assert_eq!(code, 0);
    assert_eq!(v["data"]["route_decision"]["executor"], "browser");
    assert_eq!(v["data"]["fetch_success"], true);

    assert!(
        !std::path::Path::new("/tmp/pwned_encoded").exists(),
        "decoded-looking metacharacters must not execute"
    );
}

#[test]
fn sources_command_lists_accepted_and_rejected() {
    let env = Env::new();
    env.research(&["new", "t1", "--slug", "t1", "--json"]);
    let pa_ok = env.write_fake_bin("postagent_ok", &fake_postagent_happy());
    let pa_bad = env.write_fake_bin("postagent_bad", &fake_postagent_404());

    env.research_with_bins(
        &[
            "add",
            "https://news.ycombinator.com/item?id=1",
            "--slug",
            "t1",
            "--json",
        ],
        Some(pa_ok.to_str().unwrap()),
        None,
    );
    env.research_with_bins(
        &[
            "add",
            "https://github.com/a/b",
            "--slug",
            "t1",
            "--json",
        ],
        Some(pa_bad.to_str().unwrap()),
        None,
    );

    let (v, code, _) = env.research(&["sources", "t1", "--rejected", "--json"]);
    assert_eq!(code, 0);
    let accepted = v["data"]["accepted"].as_array().unwrap();
    let rejected = v["data"]["rejected"].as_array().unwrap();
    assert_eq!(accepted.len(), 1);
    assert_eq!(rejected.len(), 1);
    assert_eq!(accepted[0]["kind"], "hn-item");
    assert_eq!(rejected[0]["reason"], "api_error");
}
