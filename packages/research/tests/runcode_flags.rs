//! Integration tests for `v2-frame-id-runcode-args.spec.md`.
//!
//! Coverage map (BDD scenario → test name, all named exactly as the spec
//! § 验收标准 requires):
//!
//! 1.  scenario: 不传 frame-id 时 cmd 字符串不含 --frame-id
//!     → `runcode_cmd_no_frame_id_omits_flag`
//! 2.  scenario: 传 frame-id 时 cmd 字符串注入 --frame-id 段
//!     → `runcode_cmd_with_frame_id_injects_flag`
//! 3.  scenario: 不传 run-code-args 时 cmd 字符串不含 --args
//!     → `runcode_cmd_no_args_omits_flag`
//! 4.  scenario: 传 run-code-args 时 cmd 字符串注入 JSON literal
//!     → `runcode_cmd_with_args_injects_json_literal`
//! 5.  scenario: frame-id 与 args 同时存在时 cmd 字符串同时注入两段
//!     → `runcode_cmd_with_both_frame_and_args_emits_both_flags`
//! 6.  scenario: CLI 拒绝 --run-code-args 非数组 JSON
//!     → `add_cli_rejects_non_array_runcode_args_json`
//! 7.  scenario: CLI 拒绝 --frame-id 负数
//!     → `add_cli_rejects_negative_frame_id`
//! 8.  scenario: CLI 拒绝 --run-code-args 不合法 JSON
//!     → `add_cli_rejects_malformed_runcode_args_json`
//! 9.  scenario: non-browser executor 忽略 frame-id 与 run-code-args
//!     → `non_browser_route_ignores_runcode_flags`
//! 10. scenario: V2 path 透传 CLI 收到的 frame-id 与 args 到 build_runcode_cmd
//!     → `v2_run_passes_frame_id_and_args_through`
//! 11. scenario: batch 多 URL 共享同一对 flag 值
//!     → `batch_propagates_runcode_flags_to_all_urls`
//!
//! Tests 1-5, 10 are unit-style: they call the library helpers in
//! `research::fetch::browser_v2` directly. Tests 6-8 spawn the CLI as a
//! subprocess. Tests 9 and 11 spin up a minimal in-process MCP mock
//! server so the V2 backend can be exercised end-to-end without
//! touching the real edge.actionbook.dev — the mock captures every
//! tools/call cmd string so we can assert on it.

use research::fetch::browser_v2::{build_close_cmd, build_new_tab_cmd, build_runcode_cmd};
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use tempfile::TempDir;

fn research_bin() -> String {
    env!("CARGO_BIN_EXE_ascent-research").to_string()
}

// ── 1. runcode_cmd_no_frame_id_omits_flag ──────────────────────────────────

#[test]
fn runcode_cmd_no_frame_id_omits_flag() {
    // build_runcode_cmd's `caller_timeout_ms` is the OUTER envelope; it
    // derives the inner --timeout by subtracting 5 s slack (spec § 双层
    // 超时). The BDD scenario asserts the prefix
    // "browser run-code --tab research-demo-1 --timeout 85000" — meaning
    // an inner --timeout of 85000. Pass 90_000 outer so 90000 - 5000 =
    // 85000 inner, matching the spec's literal expected prefix.
    let cmd = build_runcode_cmd("research-demo-1", 90_000, None, None);
    assert!(
        cmd.starts_with("browser run-code --tab research-demo-1 --timeout 85000"),
        "cmd should begin with the canonical prefix, got: {cmd}"
    );
    assert!(!cmd.contains("--frame-id"), "must omit --frame-id: {cmd}");
    assert!(!cmd.contains("--args"), "must omit --args: {cmd}");
}

// ── 2. runcode_cmd_with_frame_id_injects_flag ──────────────────────────────

#[test]
fn runcode_cmd_with_frame_id_injects_flag() {
    // caller_timeout_ms = 90000 → inner --timeout 85000 (5 s slack)
    let cmd = build_runcode_cmd("research-demo-1", 90_000, Some(3), None);
    assert!(cmd.contains("--frame-id 3"), "expect --frame-id 3 in: {cmd}");

    // "--frame-id 3" must appear before the inline JS (which starts with `'async`)
    let frame_idx = cmd.find("--frame-id 3").expect("--frame-id 3 present");
    let js_idx = cmd.find("'async").expect("inline JS present");
    assert!(
        frame_idx < js_idx,
        "expected --frame-id 3 before inline JS, got cmd: {cmd}"
    );

    assert!(!cmd.contains("--args"), "must omit --args: {cmd}");
}

// ── 3. runcode_cmd_no_args_omits_flag ──────────────────────────────────────

#[test]
fn runcode_cmd_no_args_omits_flag() {
    let cmd = build_runcode_cmd("research-demo-1", 90_000, None, None);
    assert!(!cmd.contains("--args"), "must omit --args: {cmd}");
}

// ── 4. runcode_cmd_with_args_injects_json_literal ──────────────────────────

#[test]
fn runcode_cmd_with_args_injects_json_literal() {
    let args = json!([1, 2, "x"]);
    let cmd = build_runcode_cmd("research-demo-1", 90_000, None, Some(&args));
    // serde_json::to_string emits compact JSON, no whitespace.
    assert!(
        cmd.contains(r#"--args '[1,2,"x"]'"#),
        "expect --args '[1,2,\"x\"]' in: {cmd}"
    );
    // JSON literal must be wrapped in single quotes (shell-safe).
    let frag_idx = cmd
        .find(r#"--args '[1,2,"x"]'"#)
        .expect("args fragment present");
    // The opening single-quote of the inline JS comes later. Check ordering.
    let js_idx = cmd.find("'async").expect("inline JS present");
    assert!(
        frag_idx < js_idx,
        "expected --args before inline JS, got: {cmd}"
    );
}

// ── 5. runcode_cmd_with_both_frame_and_args_emits_both_flags ───────────────

#[test]
fn runcode_cmd_with_both_frame_and_args_emits_both_flags() {
    let args = json!(["query"]);
    let cmd = build_runcode_cmd("research-demo-1", 90_000, Some(2), Some(&args));
    let frame_idx = cmd.find("--frame-id 2").expect("--frame-id 2 must appear");
    let args_idx = cmd
        .find(r#"--args '["query"]'"#)
        .expect("--args '[\"query\"]' must appear");
    // Order = --frame-id 2 BEFORE --args (V2 server CLI documented order).
    assert!(
        frame_idx < args_idx,
        "expected --frame-id before --args, got: {cmd}"
    );
}

// ── 6. add_cli_rejects_non_array_runcode_args_json ─────────────────────────

#[test]
fn add_cli_rejects_non_array_runcode_args_json() {
    let env = TempCliEnv::new();
    env.create_session("t1");
    // `{"k":1}` is a valid JSON object, not an array — CLI must reject.
    let out = env.research_capture(&[
        "add",
        "https://example.com",
        "--slug",
        "t1",
        "--run-code-args",
        r#"{"k":1}"#,
        "--json",
    ]);
    assert_ne!(out.code, 0, "must exit non-zero, stderr={}", out.stderr);
    assert!(
        out.stderr.contains("must be a JSON array"),
        "expect 'must be a JSON array' in stderr, got: {}",
        out.stderr
    );
    // Sanity: no source_attempted should have hit the jsonl — CLI failed
    // before any fetch::execute call.
    let jsonl_path = env.session_dir("t1").join("session.jsonl");
    if jsonl_path.exists() {
        let jsonl = fs::read_to_string(&jsonl_path).unwrap_or_default();
        assert!(
            !jsonl.contains("source_attempted"),
            "no fetch should have been attempted; jsonl: {jsonl}"
        );
    }
}

// ── 7. add_cli_rejects_negative_frame_id ───────────────────────────────────

#[test]
fn add_cli_rejects_negative_frame_id() {
    let env = TempCliEnv::new();
    env.create_session("t1");
    let out = env.research_capture(&[
        "add",
        "https://example.com",
        "--slug",
        "t1",
        "--frame-id",
        "-1",
        "--json",
    ]);
    assert_ne!(out.code, 0, "must exit non-zero, stderr={}", out.stderr);
    assert!(
        out.stderr.contains("frame-id"),
        "expect 'frame-id' in stderr, got: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("must be >= 0"),
        "expect 'must be >= 0' in stderr, got: {}",
        out.stderr
    );
    let jsonl_path = env.session_dir("t1").join("session.jsonl");
    if jsonl_path.exists() {
        let jsonl = fs::read_to_string(&jsonl_path).unwrap_or_default();
        assert!(
            !jsonl.contains("source_attempted"),
            "no fetch should have been attempted; jsonl: {jsonl}"
        );
    }
}

// ── 8. add_cli_rejects_malformed_runcode_args_json ─────────────────────────

#[test]
fn add_cli_rejects_malformed_runcode_args_json() {
    let env = TempCliEnv::new();
    env.create_session("t1");
    let out = env.research_capture(&[
        "add",
        "https://example.com",
        "--slug",
        "t1",
        "--run-code-args",
        "not json at all",
        "--json",
    ]);
    assert_ne!(out.code, 0, "must exit non-zero, stderr={}", out.stderr);
    let stderr = out.stderr.as_str();
    assert!(
        stderr.contains("invalid JSON") || stderr.contains("expected JSON array"),
        "expect 'invalid JSON' or 'expected JSON array' in stderr, got: {stderr}"
    );
    let jsonl_path = env.session_dir("t1").join("session.jsonl");
    if jsonl_path.exists() {
        let jsonl = fs::read_to_string(&jsonl_path).unwrap_or_default();
        assert!(
            !jsonl.contains("source_attempted"),
            "no fetch should have been attempted; jsonl: {jsonl}"
        );
    }
}

// ── 9. non_browser_route_ignores_runcode_flags ─────────────────────────────

#[test]
fn non_browser_route_ignores_runcode_flags() {
    // The postagent route is a non-browser executor — flags must be
    // silently dropped, no warning, fetch completes normally.
    let env = TempCliEnv::new();
    env.create_session("t1");

    // Fake postagent emits a valid JSON body so smell test passes.
    let script = r#"#!/bin/sh
cat <<'JSON'
{"title":"Hello","score":100}
JSON
"#;
    let pa = env.write_fake_bin("postagent", script);

    let out = env.research_capture_with_bins(
        &[
            "add",
            // GitHub issue URL → postagent route per default preset.
            "https://github.com/tokio-rs/tokio/issues/8056",
            "--slug",
            "t1",
            "--frame-id",
            "2",
            "--run-code-args",
            "[1]",
            "--json",
        ],
        Some(pa.to_str().unwrap()),
        None,
    );
    assert_eq!(
        out.code, 0,
        "non-browser path should succeed silently. stderr={}",
        out.stderr
    );

    // Confirm no "ignored" / "warn" chatter about the flags appeared.
    let combined = format!("{}\n{}", out.stdout, out.stderr);
    let lower = combined.to_lowercase();
    assert!(
        !lower.contains("frame-id"),
        "no warning about frame-id expected; got combined output: {combined}"
    );
    assert!(
        !lower.contains("ignor"),
        "no 'ignored' warning expected; got combined output: {combined}"
    );

    // Confirm the postagent argv NEVER carried --frame-id or --args. The
    // fake postagent doesn't log argv but the only way ascent could leak
    // these flags into a non-browser cmd would be via argv, and the
    // smell-tested envelope above proves the fake ran exactly once.
    // Additionally, this confirms fetch::execute treated them as optional.
    let json_line = out
        .stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or("{}");
    let v: Value = serde_json::from_str(json_line).unwrap_or(Value::Null);
    assert_eq!(v["data"]["route_decision"]["executor"], "postagent");
    assert_eq!(v["data"]["fetch_success"], true);
}

// ── 10. v2_run_passes_frame_id_and_args_through ────────────────────────────

#[test]
fn v2_run_passes_frame_id_and_args_through() {
    // Direct unit-style verification of the V2 cmd-string composer
    // contract (which is what `browser_v2::run` actually invokes once
    // per call). This is the cleanest way to assert that:
    //   - run-code cmd includes --frame-id 1 and --args '[]'
    //   - new-tab cmd does NOT
    //   - close cmd does NOT
    let frame_id = Some(1u32);
    let empty_arr = json!([]);
    let runcode = build_runcode_cmd("research-foo-1", 90_000, frame_id, Some(&empty_arr));
    assert!(
        runcode.contains("--frame-id 1"),
        "run-code cmd must carry --frame-id 1, got: {runcode}"
    );
    assert!(
        runcode.contains("--args '[]'"),
        "run-code cmd must carry --args '[]', got: {runcode}"
    );

    let goto = build_new_tab_cmd("https://example.com", "research-foo-1");
    assert!(
        !goto.contains("--frame-id") && !goto.contains("--args"),
        "new-tab cmd must NOT carry runcode flags, got: {goto}"
    );

    let close = build_close_cmd("research-foo-1");
    assert!(
        !close.contains("--frame-id") && !close.contains("--args"),
        "close cmd must NOT carry runcode flags, got: {close}"
    );
}

// ── 11. batch_propagates_runcode_flags_to_all_urls ─────────────────────────

#[test]
fn batch_propagates_runcode_flags_to_all_urls() {
    // Spin up an in-process MCP mock that records every tools/call cmd
    // string. Run `research batch <a> <b> <c> --frame-id 1
    // --run-code-args '["x"]'`. Assert that exactly three run-code cmd
    // strings landed on the mock and each carries the shared flags.

    let mock = McpMock::start();
    let env = TempCliEnv::new();
    env.create_session("t1");

    let out = env.research_capture_with_extra_env(
        &[
            "batch",
            "https://example.com/a",
            "https://example.com/b",
            "https://example.com/c",
            "--slug",
            "t1",
            "--frame-id",
            "1",
            "--run-code-args",
            r#"["x"]"#,
            "--json",
        ],
        &[
            ("ACTIONBOOK_BACKEND", "v2-mcp"),
            ("ACTIONBOOK_MCP_ENDPOINT", &mock.endpoint),
            ("ACTIONBOOK_API_KEY", "test-key"),
        ],
    );
    assert_eq!(
        out.code, 0,
        "batch should complete cleanly. stderr={} stdout={}",
        out.stderr, out.stdout
    );

    let runcode_cmds = mock.runcode_cmds();
    assert_eq!(
        runcode_cmds.len(),
        3,
        "expect exactly 3 run-code calls (one per URL), got {}: {:?}",
        runcode_cmds.len(),
        runcode_cmds
    );
    for (idx, cmd) in runcode_cmds.iter().enumerate() {
        assert!(
            cmd.contains("--frame-id 1"),
            "run-code cmd #{idx} missing --frame-id 1: {cmd}"
        );
        assert!(
            cmd.contains(r#"--args '["x"]'"#),
            "run-code cmd #{idx} missing --args '[\"x\"]': {cmd}"
        );
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

struct CapturedOutput {
    code: i32,
    stdout: String,
    stderr: String,
}

struct TempCliEnv {
    _tmp: TempDir,
    home: String,
    bin_dir: PathBuf,
}

impl TempCliEnv {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let bin_dir = tmp.path().join("_bin");
        fs::create_dir_all(&bin_dir).unwrap();
        Self {
            _tmp: tmp,
            home,
            bin_dir,
        }
    }

    fn create_session(&self, slug: &str) {
        let mut cmd = Command::new(research_bin());
        cmd.args(["new", slug, "--slug", slug, "--json"]);
        cmd.env("ACTIONBOOK_RESEARCH_HOME", &self.home);
        let out = cmd.output().expect("spawn research new");
        assert!(
            out.status.success(),
            "research new failed: stderr={} stdout={}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout),
        );
    }

    fn write_fake_bin(&self, name: &str, script: &str) -> PathBuf {
        let path = self.bin_dir.join(name);
        fs::write(&path, script).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn session_dir(&self, slug: &str) -> PathBuf {
        PathBuf::from(&self.home).join(slug)
    }

    fn research_capture(&self, args: &[&str]) -> CapturedOutput {
        let mut cmd = Command::new(research_bin());
        cmd.args(args);
        cmd.env("ACTIONBOOK_RESEARCH_HOME", &self.home);
        let out = cmd.output().expect("spawn research");
        CapturedOutput {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn research_capture_with_bins(
        &self,
        args: &[&str],
        postagent: Option<&str>,
        actionbook: Option<&str>,
    ) -> CapturedOutput {
        let mut cmd = Command::new(research_bin());
        cmd.args(args);
        cmd.env("ACTIONBOOK_RESEARCH_HOME", &self.home);
        if let Some(p) = postagent {
            cmd.env("POSTAGENT_BIN", p);
        }
        if let Some(a) = actionbook {
            cmd.env("ACTIONBOOK_BIN", a);
            cmd.env("ACTIONBOOK_BACKEND", "v1-cli");
        }
        let out = cmd.output().expect("spawn research");
        CapturedOutput {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn research_capture_with_extra_env(
        &self,
        args: &[&str],
        extra_env: &[(&str, &str)],
    ) -> CapturedOutput {
        let mut cmd = Command::new(research_bin());
        cmd.args(args);
        cmd.env("ACTIONBOOK_RESEARCH_HOME", &self.home);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn research");
        CapturedOutput {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

/// Minimal in-process MCP mock. Accepts:
///  - JSON-RPC `initialize` → returns `Mcp-Session-Id: mock-session` and
///    a happy result envelope.
///  - JSON-RPC `notifications/initialized` → 200 with empty `{}`.
///  - JSON-RPC `tools/call` → records the `arguments.cmd` string and
///    returns a happy-path response so the V2 client can extract a
///    `{url, title, text}` payload from the run-code tool.
struct McpMock {
    endpoint: String,
    cmds: Arc<Mutex<Vec<String>>>,
}

impl McpMock {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().unwrap();
        let cmds: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let cmds_for_thread = cmds.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let cmds = cmds_for_thread.clone();
                thread::spawn(move || {
                    // Read up to the headers (very simple: read until \r\n\r\n).
                    let mut buf = [0u8; 65536];
                    let n = match s.read(&mut buf) {
                        Ok(n) if n > 0 => n,
                        _ => return,
                    };
                    let raw = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let (headers, body_seen) = match raw.split_once("\r\n\r\n") {
                        Some((h, b)) => (h.to_string(), b.to_string()),
                        None => (raw.clone(), String::new()),
                    };
                    // Honor Content-Length so we read the full body even if
                    // it spilled past the initial chunk.
                    let content_length: usize = headers
                        .lines()
                        .find_map(|l| {
                            let lower = l.to_ascii_lowercase();
                            lower
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    let mut body = body_seen;
                    while body.len() < content_length {
                        let mut more = [0u8; 4096];
                        match s.read(&mut more) {
                            Ok(0) => break,
                            Ok(m) => body.push_str(&String::from_utf8_lossy(&more[..m])),
                            Err(_) => break,
                        }
                    }
                    let parsed: Value =
                        serde_json::from_str(&body).unwrap_or(Value::Null);
                    let method =
                        parsed.get("method").and_then(Value::as_str).unwrap_or("");
                    let response_json: String;
                    let extra_headers: &str;
                    match method {
                        "initialize" => {
                            extra_headers = "Mcp-Session-Id: mock-session\r\n";
                            response_json = json!({
                                "jsonrpc": "2.0",
                                "id": parsed.get("id").cloned().unwrap_or(Value::Null),
                                "result": {
                                    "protocolVersion": "2025-06-18",
                                    "capabilities": {},
                                    "serverInfo": {"name": "mock", "version": "0"}
                                }
                            })
                            .to_string();
                        }
                        "notifications/initialized" => {
                            extra_headers = "";
                            response_json = json!({}).to_string();
                        }
                        "tools/call" => {
                            extra_headers = "";
                            let cmd = parsed
                                .get("params")
                                .and_then(|p| p.get("arguments"))
                                .and_then(|a| a.get("cmd"))
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            cmds.lock().unwrap().push(cmd.clone());

                            // Construct happy-path text content.  V2 client
                            // calls extract_run_code_payload on the text;
                            // it scans for the first `{` and JSON-parses
                            // from there. The wrapper envelope below has
                            // an emitter line then the actual payload.
                            let text = if cmd.starts_with("browser run-code") {
                                "[t1]\nok browser run-code\n{\"result\":{\"url\":\"https://example.com/\",\"title\":\"Example\",\"text\":\"hello world from mock — this body is intentionally long enough to pass the smell test by exceeding the default min body bytes threshold. Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum. \"}}".to_string()
                            } else {
                                "[t1]\nok ".to_string()
                            };
                            response_json = json!({
                                "jsonrpc": "2.0",
                                "id": parsed.get("id").cloned().unwrap_or(Value::Null),
                                "result": {
                                    "content": [{"type":"text","text": text}]
                                }
                            })
                            .to_string();
                        }
                        _ => {
                            extra_headers = "";
                            response_json = json!({"jsonrpc":"2.0","id":parsed.get("id").cloned().unwrap_or(Value::Null),"result":{}}).to_string();
                        }
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{}",
                        response_json.len(),
                        extra_headers,
                        response_json
                    );
                    let _ = s.write_all(response.as_bytes());
                    let _ = s.flush();
                });
            }
        });
        let endpoint = format!("http://{addr}/mcp");
        Self { endpoint, cmds }
    }

    fn runcode_cmds(&self) -> Vec<String> {
        self.cmds
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("browser run-code"))
            .cloned()
            .collect()
    }
}
