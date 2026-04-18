//! Session state: on-disk layout, slug rules, event log schema, active pointer.
//!
//! No subprocess invocations, no network. Pure filesystem operations
//! and data-shape definitions. `commands::*` builds on top of this layer.

pub mod active;
pub mod config;
pub mod event;
pub mod layout;
pub mod log;
pub mod md_template;
pub mod slug;
pub mod sources_block;
