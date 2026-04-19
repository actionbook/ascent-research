//! Autonomous research loop.
//!
//! Entire module is gated behind the `autoresearch` feature. Default builds
//! of `research` do not pull in the LLM-facing dependency stack. See
//! `specs/research-autonomous-loop.spec.md` for the design contract.
//!
//! Provider impls live in their own sub-feature:
//! - `provider-claude` → `claude.rs`  (uses `cc-sdk`)
//! - `provider-codex`  → `codex.rs`   (spawns `codex app-server`)
//!
//! A `FakeProvider` is always compiled under `autoresearch` so tests never
//! touch a real LLM.

pub mod provider;
pub mod schema;

#[cfg(feature = "provider-claude")]
pub mod claude;

#[cfg(feature = "provider-codex")]
pub mod codex;
