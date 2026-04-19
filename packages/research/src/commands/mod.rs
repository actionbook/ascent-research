//! Subcommand handlers. Each module exposes `run() -> Envelope`. In MVP #1
//! every handler returns `NOT_IMPLEMENTED` via `output::not_implemented`.
//! Real logic lands in subsequent specs.

pub mod add;
pub mod batch;
pub mod close;
pub mod list;
pub mod new;
pub mod report;
pub mod resume;
pub mod rm;
pub mod route;
pub mod series;
pub mod show;
pub mod sources;
pub mod status;
pub mod synthesize;
