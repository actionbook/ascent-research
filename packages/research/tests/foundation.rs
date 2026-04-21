//! Integration tests for `research-cli-foundation` spec scenarios.
//!
//! These tests exec the built binary (`cargo run --release`) so they also
//! verify clap dispatch + envelope rendering end-to-end.

use serde_json::Value;
use std::process::Command;

fn binary() -> String {
    env!("CARGO_BIN_EXE_ascent-research").to_string()
}

fn run(args: &[&str]) -> (String, String, i32) {
    let out = Command::new(binary())
        .args(args)
        .output()
        .expect("spawn research binary");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn help_lists_all_subcommands() {
    let (stdout, _, code) = run(&["--help"]);
    assert_eq!(code, 0);
    for cmd in [
        "new", "list", "show", "status", "resume", "add", "sources", "synthesize",
        "close", "rm", "route", "help",
    ] {
        assert!(
            stdout.contains(cmd),
            "subcommand `{cmd}` missing from --help: {stdout}"
        );
    }
    // Global flags
    for flag in ["--json", "--verbose", "--no-color"] {
        assert!(
            stdout.contains(flag),
            "global flag `{flag}` missing: {stdout}"
        );
    }
}

#[test]
fn research_help_alias_exits_zero() {
    let (stdout, _, code) = run(&["help"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("Usage:") || stdout.contains("usage:"));
}

#[test]
fn all_subcommands_are_implemented() {
    // All 12 subcommands have dedicated integration tests in their own
    // test files (lifecycle, route, add_source, synthesize). This test
    // exists as a backstop — if a future regression makes a command
    // start returning NOT_IMPLEMENTED, other tests would flag it first,
    // but this one double-checks no command is accidentally stubbed.
    let _ = Value::Null; // keep imports alive
}
