//! Fetch layer: subprocess wrappers + smell test + result types.
//!
//! All external I/O lives here. Command handlers (`commands::add`)
//! orchestrate, but never spawn subprocess or parse response JSON directly.

pub mod browser;
pub mod postagent;
pub mod smell;

use serde::Serialize;

use crate::session::event::RejectReason;

/// Raw output captured from a fetch subprocess (postagent or actionbook).
#[derive(Debug, Clone)]
pub struct RawFetch {
    /// The exact bytes the subprocess wrote to stdout (may be decoded JSON).
    pub raw_stdout: Vec<u8>,
    /// Subprocess stderr (saved for .rejected.json debug).
    pub raw_stderr: Vec<u8>,
    /// Exit code (0 on clean exit).
    pub exit_code: i32,
    /// Wall-clock duration.
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FetchOutcome {
    pub accepted: bool,
    pub observed_url: Option<String>,
    pub observed_bytes: u64,
    pub reject_reason: Option<RejectReason>,
    pub warnings: Vec<String>,
    /// Body length as reported / derived, for envelope `bytes` field.
    pub bytes: u64,
}
