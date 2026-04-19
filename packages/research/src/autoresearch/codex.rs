//! `provider-codex` — CodexProvider speaking JSON-RPC 2.0 over stdio to
//! `codex app-server`.
//!
//! Protocol reference:
//! https://github.com/openai/codex/tree/main/codex-rs/app-server#protocol
//!
//! Scaffold only. The actual spawn + initialize handshake + chat exchange
//! lands in the executor work that comes next. This file establishes the
//! type + feature gate so the build matrix is green and downstream
//! modules can reference `CodexProvider` by name.

use super::provider::{AgentProvider, ProviderError};
use async_trait::async_trait;

pub struct CodexProvider {
    _private: (),
}

impl CodexProvider {
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl Default for CodexProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentProvider for CodexProvider {
    async fn ask(&self, _system: &str, _user: &str) -> Result<String, ProviderError> {
        // TODO: spawn `codex app-server --listen stdio://`, run the JSON-RPC
        // 2.0 initialize handshake, send a chat request, collect the reply.
        // Track in follow-up per specs/research-autonomous-loop.spec.md.
        Err(ProviderError::NotAvailable(
            "CodexProvider not yet wired — blocks loop execution under \
             `--features provider-codex`. Use --provider fake for now."
                .to_string(),
        ))
    }

    fn name(&self) -> &'static str {
        "codex"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn codex_provider_reports_not_available_for_now() {
        let p = CodexProvider::new();
        assert_eq!(p.name(), "codex");
        match p.ask("sys", "usr").await {
            Err(ProviderError::NotAvailable(msg)) => {
                assert!(msg.contains("not yet wired"));
            }
            other => panic!("expected NotAvailable, got {other:?}"),
        }
    }
}
