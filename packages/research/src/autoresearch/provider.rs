//! `AgentProvider` trait + the `FakeProvider` used by tests.
//!
//! Real providers (Claude / Codex) live in sibling modules and are each
//! gated by their own Cargo feature. Test code only ever uses `FakeProvider`
//! so the test suite never makes a real LLM call.

use async_trait::async_trait;
use std::sync::Mutex;

#[derive(Debug)]
pub enum ProviderError {
    /// Provider binary / daemon not available (cc-sdk init failure, codex
    /// not on PATH, etc.). Caller maps to `PROVIDER_NOT_AVAILABLE`.
    NotAvailable(String),
    /// The call reached the provider but the provider itself returned an
    /// error (rate limit, auth, transport). Caller may treat as retryable.
    CallFailed(String),
    /// Response arrived but the text is empty / unusable before schema
    /// parsing. Caller maps to `LLM_SCHEMA_VIOLATION` after skip.
    EmptyResponse,
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderError::NotAvailable(s) => write!(f, "provider not available: {s}"),
            ProviderError::CallFailed(s) => write!(f, "provider call failed: {s}"),
            ProviderError::EmptyResponse => write!(f, "provider returned empty response"),
        }
    }
}
impl std::error::Error for ProviderError {}

#[async_trait]
pub trait AgentProvider: Send + Sync {
    /// Single non-interactive exchange: given `system` prompt + `user`
    /// prompt, return the provider's raw text response. Schema validation
    /// is the caller's job.
    async fn ask(&self, system: &str, user: &str) -> Result<String, ProviderError>;

    /// Stable name for envelope / logging. Expected values: "claude",
    /// "codex", "fake".
    fn name(&self) -> &'static str;
}

/// Test-only provider that replays a fixed sequence of responses in order.
/// Once exhausted, subsequent calls return `ProviderError::EmptyResponse`
/// so tests that push past the scripted turns fail loudly.
///
/// `FakeProvider` is compiled under any `autoresearch` build (not just
/// test configs) so it can also serve as a manual `--provider fake` knob
/// during local development.
pub struct FakeProvider {
    responses: Mutex<std::collections::VecDeque<String>>,
}

impl FakeProvider {
    pub fn new<I, S>(responses: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            responses: Mutex::new(responses.into_iter().map(Into::into).collect()),
        }
    }

    pub fn remaining(&self) -> usize {
        self.responses.lock().unwrap().len()
    }
}

#[async_trait]
impl AgentProvider for FakeProvider {
    async fn ask(&self, _system: &str, _user: &str) -> Result<String, ProviderError> {
        let mut q = self.responses.lock().unwrap();
        q.pop_front().ok_or(ProviderError::EmptyResponse)
    }

    fn name(&self) -> &'static str {
        "fake"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_provider_returns_in_order() {
        let p = FakeProvider::new([
            r#"{"reasoning":"first","actions":[],"done":false}"#,
            r#"{"reasoning":"second","actions":[],"done":true,"reason":"stop"}"#,
        ]);
        assert_eq!(p.remaining(), 2);
        assert_eq!(p.name(), "fake");

        let first = p.ask("sys", "usr").await.unwrap();
        assert!(first.contains("first"));
        assert_eq!(p.remaining(), 1);

        let second = p.ask("sys", "usr").await.unwrap();
        assert!(second.contains("second"));
        assert_eq!(p.remaining(), 0);
    }

    #[tokio::test]
    async fn fake_provider_exhausts_with_empty_response() {
        let p = FakeProvider::new(Vec::<String>::new());
        match p.ask("sys", "usr").await {
            Err(ProviderError::EmptyResponse) => {}
            other => panic!("expected EmptyResponse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fake_provider_error_after_drain() {
        let p = FakeProvider::new([r#"{"reasoning":"ok","actions":[],"done":true}"#]);
        let _ = p.ask("", "").await.unwrap();
        // Second call should error — scripts are finite.
        assert!(p.ask("", "").await.is_err());
    }
}
