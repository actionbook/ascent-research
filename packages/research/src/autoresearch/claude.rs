//! `provider-claude` — ClaudeProvider backed by the cc-sdk crate.
//!
//! cc-sdk wraps the Claude Code CLI's non-interactive query mode. It relies
//! on the local Claude Code installation's authentication state; no API
//! key is stored by research-rs itself.
//!
//! Scaffold: the actual wiring lives in the executor work that comes next;
//! this file establishes the type + feature gate so the build matrix is
//! green and downstream modules can reference `ClaudeProvider` by name.

use super::provider::{AgentProvider, ProviderError};
use async_trait::async_trait;

/// Claude Code backed provider. Construction is cheap — the cc-sdk client
/// is built lazily on first `ask()` so `research loop --provider claude`
/// fails fast with a clear error on misconfiguration.
pub struct ClaudeProvider {
    _private: (),
}

impl ClaudeProvider {
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl Default for ClaudeProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentProvider for ClaudeProvider {
    async fn ask(&self, _system: &str, _user: &str) -> Result<String, ProviderError> {
        // TODO: wire cc-sdk client. Track in follow-up per
        // specs/research-autonomous-loop.spec.md §"ClaudeProvider".
        Err(ProviderError::NotAvailable(
            "ClaudeProvider not yet wired — blocks loop execution under \
             `--features provider-claude`. Use --provider fake for now."
                .to_string(),
        ))
    }

    fn name(&self) -> &'static str {
        "claude"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn claude_provider_reports_not_available_for_now() {
        let p = ClaudeProvider::new();
        assert_eq!(p.name(), "claude");
        match p.ask("sys", "usr").await {
            Err(ProviderError::NotAvailable(msg)) => {
                assert!(msg.contains("not yet wired"));
            }
            other => panic!("expected NotAvailable, got {other:?}"),
        }
    }
}
