//! `provider-codex` — CodexProvider speaking JSON-RPC 2.0 over stdio to
//! `codex app-server`.
//!
//! Protocol reference:
//! https://github.com/openai/codex/tree/main/codex-rs/app-server#protocol
//!
//! Lifecycle per `ask()` call:
//! 1. spawn `codex app-server --listen stdio://` with piped stdin/stdout
//! 2. send `initialize` request + `initialized` notification
//! 3. `thread/start` → capture `threadId`
//! 4. `turn/start` with the user prompt (system is prefixed in text)
//! 5. read the event stream, concatenate every assistant-message text,
//!    return when `turn/completed` arrives
//! 6. kill the subprocess
//!
//! Keep each `ask()` in its own thread — loop iterations are independent
//! and don't need cross-turn context (the research session itself is the
//! durable context, not codex's thread).

use super::provider::{AgentProvider, ProviderError};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::time::{timeout, Duration};

/// How long we're willing to wait for the whole exchange. Codex turns can
/// be slow (10-30 s typical) but not minutes.
const TURN_TIMEOUT: Duration = Duration::from_secs(120);

/// Codex-backed provider. Spawns a fresh `codex app-server` per call —
/// simpler and safer than reusing connections across iterations.
pub struct CodexProvider {
    binary: String,
}

impl CodexProvider {
    pub fn new() -> Self {
        Self {
            binary: std::env::var("CODEX_BIN").unwrap_or_else(|_| "codex".to_string()),
        }
    }

    /// Override the codex binary path — used by tests that point at a
    /// stub app-server.
    pub fn with_binary(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
        }
    }
}

impl Default for CodexProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentProvider for CodexProvider {
    async fn ask(&self, system: &str, user: &str) -> Result<String, ProviderError> {
        timeout(TURN_TIMEOUT, self.exchange(system, user))
            .await
            .map_err(|_| ProviderError::CallFailed(format!(
                "codex exchange exceeded {}s",
                TURN_TIMEOUT.as_secs()
            )))?
    }

    fn name(&self) -> &'static str {
        "codex"
    }
}

impl CodexProvider {
    async fn exchange(&self, system: &str, user: &str) -> Result<String, ProviderError> {
        let mut child = Command::new(&self.binary)
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => ProviderError::NotAvailable(format!(
                    "codex binary '{}' not on PATH (install codex or set CODEX_BIN)",
                    self.binary
                )),
                _ => ProviderError::CallFailed(format!("spawn codex: {e}")),
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ProviderError::CallFailed("codex stdin unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProviderError::CallFailed("codex stdout unavailable".into()))?;
        let mut reader = BufReader::new(stdout).lines();
        let mut writer = stdin;

        // ── Handshake ─────────────────────────────────────────────────
        send(&mut writer, &build_initialize(1)).await?;
        // Wait for the matching `id: 1` response.
        wait_for_response(&mut reader, 1).await?;
        send(&mut writer, &build_initialized_notification()).await?;

        // ── Start thread ──────────────────────────────────────────────
        send(&mut writer, &build_thread_start(2, system)).await?;
        let thread_resp = wait_for_response(&mut reader, 2).await?;
        let thread_id = thread_resp["result"]["threadId"]
            .as_str()
            .ok_or_else(|| {
                ProviderError::CallFailed(format!(
                    "thread/start response missing threadId: {thread_resp}"
                ))
            })?
            .to_string();

        // ── Start turn ────────────────────────────────────────────────
        // Prefix the system preamble onto the user text — Codex's
        // `turn/start` doesn't accept a separate system prompt on every
        // call; the thread-level personality is the closest analog.
        let combined = format!("[system]\n{system}\n\n[user]\n{user}");
        send(&mut writer, &build_turn_start(3, &thread_id, &combined)).await?;
        wait_for_response(&mut reader, 3).await?; // initial turn object

        // ── Stream events until turn/completed ────────────────────────
        let mut assistant_text = String::new();
        let mut turn_status: Option<String> = None;
        loop {
            let Some(line) = read_next_line(&mut reader).await? else {
                break;
            };
            let msg: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            match msg.get("method").and_then(Value::as_str) {
                Some("item/completed") => {
                    if let Some(text) = extract_assistant_text(&msg) {
                        assistant_text.push_str(&text);
                    }
                }
                Some("turn/completed") => {
                    turn_status = msg["params"]["turn"]["status"]
                        .as_str()
                        .map(str::to_string);
                    break;
                }
                _ => {}
            }
        }

        let _ = writer.shutdown().await;
        let _ = child.start_kill();
        let _ = child.wait().await;

        match turn_status.as_deref() {
            Some("completed") => {
                if assistant_text.trim().is_empty() {
                    Err(ProviderError::EmptyResponse)
                } else {
                    Ok(assistant_text)
                }
            }
            Some(other) => Err(ProviderError::CallFailed(format!(
                "codex turn status '{other}' — see codex stderr for details"
            ))),
            None => Err(ProviderError::CallFailed(
                "codex stream closed without turn/completed".into(),
            )),
        }
    }
}

// ── JSON-RPC plumbing ───────────────────────────────────────────────────

fn build_initialize(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "clientInfo": {
                "name": "research-rs",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {},
        },
    })
}

fn build_initialized_notification() -> Value {
    json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} })
}

fn build_thread_start(id: u64, _system: &str) -> Value {
    // We deliberately omit cwd / approvalPolicy / sandbox — we never ask
    // codex to execute tools, only to return text. Using the user's own
    // config defaults keeps us out of the permission prompt path.
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "thread/start",
        "params": {
            "approvalPolicy": "never",
            "sessionStartSource": "startup",
        },
    })
}

fn build_turn_start(id: u64, thread_id: &str, text: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "turn/start",
        "params": {
            "threadId": thread_id,
            "input": [{ "type": "text", "text": text }],
        },
    })
}

fn extract_assistant_text(msg: &Value) -> Option<String> {
    // `item/completed` notifications may carry an assistant message body at
    // `params.item.text` (raw text) or `params.item.content` (rich). We
    // accept both and stringify `content` as a fallback.
    let item = msg.get("params")?.get("item")?;
    if item.get("type").and_then(Value::as_str) != Some("assistantMessage") {
        return None;
    }
    if let Some(text) = item.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    item.get("content").map(|v| v.to_string())
}

async fn send(writer: &mut ChildStdin, msg: &Value) -> Result<(), ProviderError> {
    let mut line = serde_json::to_string(msg).unwrap_or_default();
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|e| ProviderError::CallFailed(format!("write codex stdin: {e}")))?;
    writer
        .flush()
        .await
        .map_err(|e| ProviderError::CallFailed(format!("flush codex stdin: {e}")))
}

async fn read_next_line(
    reader: &mut tokio::io::Lines<BufReader<ChildStdout>>,
) -> Result<Option<String>, ProviderError> {
    reader
        .next_line()
        .await
        .map_err(|e| ProviderError::CallFailed(format!("read codex stdout: {e}")))
}

async fn wait_for_response(
    reader: &mut tokio::io::Lines<BufReader<ChildStdout>>,
    expected_id: u64,
) -> Result<Value, ProviderError> {
    loop {
        let Some(line) = read_next_line(reader).await? else {
            return Err(ProviderError::CallFailed(
                "codex stream closed before response".into(),
            ));
        };
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if msg.get("id").and_then(Value::as_u64) == Some(expected_id) {
            if let Some(err) = msg.get("error") {
                return Err(ProviderError::CallFailed(format!(
                    "codex JSON-RPC error: {err}"
                )));
            }
            return Ok(msg);
        }
        // Skip notifications while waiting for this id's response.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_carries_client_info() {
        let msg = build_initialize(1);
        assert_eq!(msg["jsonrpc"], "2.0");
        assert_eq!(msg["id"], 1);
        assert_eq!(msg["method"], "initialize");
        assert_eq!(msg["params"]["clientInfo"]["name"], "research-rs");
        assert!(msg["params"]["clientInfo"]["version"]
            .as_str()
            .map(|s| !s.is_empty())
            .unwrap_or(false));
    }

    #[test]
    fn initialized_is_a_notification_without_id() {
        let msg = build_initialized_notification();
        assert_eq!(msg["method"], "initialized");
        assert!(msg.get("id").is_none(), "notifications must not carry id");
    }

    #[test]
    fn thread_start_uses_never_approval() {
        let msg = build_thread_start(2, "system");
        assert_eq!(msg["method"], "thread/start");
        assert_eq!(msg["params"]["approvalPolicy"], "never");
    }

    #[test]
    fn turn_start_wraps_text_as_input_item() {
        let msg = build_turn_start(3, "thr_abc", "hello codex");
        assert_eq!(msg["method"], "turn/start");
        assert_eq!(msg["params"]["threadId"], "thr_abc");
        let input = &msg["params"]["input"][0];
        assert_eq!(input["type"], "text");
        assert_eq!(input["text"], "hello codex");
    }

    #[test]
    fn extract_assistant_text_picks_text_field_over_content() {
        let msg = json!({
            "method": "item/completed",
            "params": {
                "item": {
                    "type": "assistantMessage",
                    "text": "direct text body",
                    "content": [{"type":"text","text":"nested"}],
                }
            }
        });
        assert_eq!(
            extract_assistant_text(&msg).as_deref(),
            Some("direct text body")
        );
    }

    #[test]
    fn extract_assistant_text_falls_back_to_content() {
        let msg = json!({
            "method": "item/completed",
            "params": {
                "item": {
                    "type": "assistantMessage",
                    "content": [{"type":"text","text":"nested body"}]
                }
            }
        });
        let got = extract_assistant_text(&msg).unwrap();
        assert!(got.contains("nested body"));
    }

    #[test]
    fn extract_assistant_text_ignores_non_assistant_items() {
        let msg = json!({
            "method": "item/completed",
            "params": { "item": { "type": "userMessage", "text": "ignored" } }
        });
        assert!(extract_assistant_text(&msg).is_none());
    }

    #[tokio::test]
    async fn codex_provider_name_is_codex() {
        let p = CodexProvider::new();
        assert_eq!(p.name(), "codex");
    }

    #[tokio::test]
    async fn codex_missing_binary_returns_not_available() {
        let p = CodexProvider::with_binary("/nonexistent/path/to/codex-does-not-exist");
        match p.ask("sys", "usr").await {
            Err(ProviderError::NotAvailable(msg)) => {
                assert!(msg.contains("codex") || msg.contains("PATH"));
            }
            other => panic!("expected NotAvailable, got {other:?}"),
        }
    }
}
