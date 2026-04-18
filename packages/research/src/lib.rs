//! `research` CLI library crate. All logic lives here; `main.rs` is a thin
//! entrypoint that dispatches to `cli::run`.
//!
//! Module structure:
//! - `cli`      — argument parsing + command dispatch
//! - `session`  — session directory layout, event log, active pointer
//! - `commands` — subcommand handlers (all stubs in MVP #1)
//! - `output`   — ActionResult / envelope helpers for --json vs plain text

#![warn(clippy::all)]

pub mod cli;
pub mod commands;
pub mod fetch;
pub mod output;
pub mod route;
pub mod session;
