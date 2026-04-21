//! `provider-claude` — ClaudeProvider backed by the [cc-sdk] crate.
//!
//! cc-sdk's `llm::query` strips the Claude Code agent layer (tools / hooks /
//! default system prompt) and gives direct LLM access using the user's
//! existing Claude Code subscription. research-rs itself stores no API key.
//!
//! [cc-sdk]: https://crates.io/crates/cc-sdk

use super::provider::{AgentProvider, ProviderError};
use async_trait::async_trait;
use cc_sdk::llm::{self, LlmOptions};

/// Claude Code backed provider.
///
/// Construction is cheap; the cc-sdk client is initialized per-request via
/// `llm::query`, so configuration errors surface immediately on the first
/// `ask()` rather than at process start.
pub struct ClaudeProvider {
    /// Optional cc-sdk `LlmOptions` — e.g., to pin a specific Claude model.
    /// None means "use cc-sdk defaults", which is the documented happy path.
    options: Option<LlmOptions>,
}

impl ClaudeProvider {
    pub fn new() -> Self {
        Self { options: None }
    }

    /// Pin a specific model or set budgets via cc-sdk's option builder.
    /// Currently unused by the default CLI path, but kept so a future
    /// `--model` / `--max-budget-usd` flag can plug in without changing
    /// the trait surface.
    #[allow(dead_code)]
    pub fn with_options(options: LlmOptions) -> Self {
        Self {
            options: Some(options),
        }
    }
}

impl Default for ClaudeProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentProvider for ClaudeProvider {
    async fn ask(&self, system: &str, user: &str) -> Result<String, ProviderError> {
        // cc-sdk's llm::query strips the agent layer and takes one prompt
        // string + optional LlmOptions. We stuff `system` into the options'
        // system_prompt field and send `user` as the prompt. Preserved any
        // pinned options (e.g., `.model(...)`) from `with_options`.
        let mut builder = LlmOptions::builder().system_prompt(system);
        if let Some(pinned) = &self.options {
            if let Some(model) = pinned.model.as_ref() {
                builder = builder.model(model.clone());
            }
        }
        let opts = builder.build();

        let response = llm::query(user, Some(opts))
            .await
            .map_err(|e| ProviderError::CallFailed(format!("cc-sdk: {e}")))?;

        if response.text.trim().is_empty() {
            return Err(ProviderError::EmptyResponse);
        }

        Ok(response.text)
    }

    fn name(&self) -> &'static str {
        "claude"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_provider_name_is_claude() {
        let p = ClaudeProvider::new();
        assert_eq!(p.name(), "claude");
    }

    // Note: no `#[tokio::test]` that actually calls cc-sdk — the real
    // provider hits the user's Claude Code subscription, which is a live
    // network dependency. That's verified by manual smoke against
    // `research loop <slug> --provider claude`. Unit tests focus on the
    // pure plumbing that doesn't touch the network.

    #[test]
    fn provider_constructor_does_not_panic() {
        let _p = ClaudeProvider::new();
        let _p2 = ClaudeProvider::default();
    }
}
