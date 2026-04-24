//! Subcommand handlers. Each module exposes `run() -> Envelope`. In MVP #1
//! every handler returns `NOT_IMPLEMENTED` via `output::not_implemented`.
//! Real logic lands in subsequent specs.

pub mod add;
pub mod add_local;
pub mod audit;
pub mod batch;
pub mod close;
pub mod coverage;
pub mod diff;
pub mod doctor;
pub mod list;
#[cfg(feature = "autoresearch")]
pub mod loop_cmd;
pub mod new;
pub mod report;
pub mod resume;
pub mod rm;
pub mod route;
pub mod schema;
pub mod series;
pub mod show;
pub mod sources;
pub mod status;
pub mod synthesize;
pub mod wiki;
pub mod wiki_lint;
pub mod wiki_query;
