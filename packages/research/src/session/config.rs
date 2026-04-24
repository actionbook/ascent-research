//! Read / write session.toml — per-session configuration.
//!
//! Schema:
//! ```toml
//! slug = "..."
//! topic = "..."
//! preset = "tech"
//! created_at = "2026-04-19T..."      # RFC3339 UTC
//! max_sources = 20                    # optional
//! closed_at = "2026-04-19T..."        # optional, set by `research close`
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use super::layout;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionConfig {
    pub slug: String,
    pub topic: String,
    pub preset: String,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_sources: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    /// Slug of the parent session if this one was created via `--from`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_slug: Option<String>,
    /// Free-form tags for grouping into series.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl SessionConfig {
    pub fn new(
        slug: impl Into<String>,
        topic: impl Into<String>,
        preset: impl Into<String>,
    ) -> Self {
        Self {
            slug: slug.into(),
            topic: topic.into(),
            preset: preset.into(),
            created_at: Utc::now(),
            max_sources: None,
            closed_at: None,
            parent_slug: None,
            tags: Vec::new(),
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed_at.is_some()
    }
}

pub fn read(slug: &str) -> std::io::Result<SessionConfig> {
    let path = layout::session_toml(slug);
    let text = fs::read_to_string(&path)?;
    toml::from_str(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub fn write(slug: &str, cfg: &SessionConfig) -> std::io::Result<()> {
    let path = layout::session_toml(slug);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(cfg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(&path, text)
}

pub fn exists(slug: &str) -> bool {
    Path::new(&layout::session_toml(slug)).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Round-trip coverage for config read/write is handled by the integration
    // tests in tests/lifecycle.rs (which spawn isolated subprocesses with
    // ACTIONBOOK_RESEARCH_HOME each). Unit-level env-var manipulation races
    // under parallel test execution, so we don't exercise it here.

    #[test]
    fn toml_round_trip_in_memory() {
        let cfg = SessionConfig::new("foo", "topic one", "tech");
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: SessionConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.slug, "foo");
        assert_eq!(back.topic, "topic one");
        assert_eq!(back.preset, "tech");
        assert!(!back.is_closed());
    }
}
