//! Integration tests for `research-cli-foundation` spec scenarios.
//!
//! These tests exec the built binary (`cargo run --release`) so they also
//! verify clap dispatch + envelope rendering end-to-end.

use serde_json::Value;
use std::process::Command;

fn binary() -> String {
    env!("CARGO_BIN_EXE_research").to_string()
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
fn remaining_stubs_return_not_implemented_json() {
    // After lifecycle MVP (#2), session commands are live. Only route /
    // add / sources / synthesize remain stubs until their respective specs
    // implement them.
    let stubs: &[(&[&str], &str)] = &[
        (&["synthesize", "--json"], "research synthesize"),
    ];
    for (args, expected_cmd) in stubs {
        let (stdout, _, code) = run(args);
        assert_ne!(code, 0, "stub {expected_cmd} should exit non-zero");
        let v: Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("stub {expected_cmd} stdout not valid JSON: {stdout} ({e})"));
        assert_eq!(v["ok"], Value::Bool(false));
        assert_eq!(v["command"], Value::String(expected_cmd.to_string()));
        assert_eq!(v["error"]["code"], Value::String("NOT_IMPLEMENTED".into()));
        assert_eq!(v["context"]["command"], Value::String(expected_cmd.to_string()));
    }
}
