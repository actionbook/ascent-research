//! `research loop <slug>` — run the autonomous research loop.
//!
//! Feature: `autoresearch`. Default builds do not include this command.

use serde_json::json;

#[cfg(feature = "provider-claude")]
use crate::autoresearch::claude::ClaudeProvider;
#[cfg(feature = "provider-codex")]
use crate::autoresearch::codex::CodexProvider;
use crate::autoresearch::executor::{self, LoopConfig};
use crate::autoresearch::provider::{AgentProvider, FakeProvider};
use crate::output::Envelope;
use crate::session::{active, config};

const CMD: &str = "research loop";

pub fn run(
    slug_arg: Option<&str>,
    provider_name: &str,
    iterations: Option<u32>,
    max_actions: Option<u32>,
    dry_run: bool,
    fake_responses: Option<Vec<String>>,
) -> Envelope {
    let slug = match slug_arg {
        Some(s) => s.to_string(),
        None => match active::get_active() {
            Some(s) => s,
            None => {
                return Envelope::fail(
                    CMD,
                    "NO_ACTIVE_SESSION",
                    "no active session — pass <slug> or run `research new` first",
                );
            }
        },
    };

    if !config::exists(&slug) {
        return Envelope::fail(CMD, "SESSION_NOT_FOUND", format!("no session '{slug}'"))
            .with_context(json!({ "session": slug }));
    }

    let cfg = LoopConfig {
        iterations: iterations.unwrap_or(executor::DEFAULT_ITERATIONS),
        max_actions: max_actions.unwrap_or(executor::DEFAULT_MAX_ACTIONS),
        dry_run,
    };

    let provider: Box<dyn AgentProvider> = match provider_name {
        "fake" => {
            let responses = fake_responses.unwrap_or_else(|| {
                vec![r#"{"reasoning":"no provider responses provided","actions":[],"done":true,"reason":"fake drained"}"#.to_string()]
            });
            Box::new(FakeProvider::new(responses))
        }
        #[cfg(feature = "provider-claude")]
        "claude" => Box::new(ClaudeProvider::new()),
        #[cfg(not(feature = "provider-claude"))]
        "claude" => {
            return Envelope::fail(
                CMD,
                "PROVIDER_NOT_AVAILABLE",
                "provider 'claude' requires the `provider-claude` feature (build with `--features provider-claude`)",
            )
            .with_context(json!({ "session": slug }));
        }
        #[cfg(feature = "provider-codex")]
        "codex" => Box::new(CodexProvider::new()),
        #[cfg(not(feature = "provider-codex"))]
        "codex" => {
            return Envelope::fail(
                CMD,
                "PROVIDER_NOT_AVAILABLE",
                "provider 'codex' requires the `provider-codex` feature (build with `--features provider-codex`)",
            )
            .with_context(json!({ "session": slug }));
        }
        other => {
            return Envelope::fail(
                CMD,
                "PROVIDER_NOT_AVAILABLE",
                format!("unknown provider '{other}'; expected one of: fake, claude, codex"),
            )
            .with_context(json!({ "session": slug }));
        }
    };

    let bin = std::env::current_exe().unwrap_or_else(|_| "research".into());
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            return Envelope::fail(CMD, "IO_ERROR", format!("build tokio runtime: {e}"))
                .with_context(json!({ "session": slug }));
        }
    };
    let report = rt.block_on(executor::run(&*provider, &slug, cfg, &bin));

    Envelope::ok(
        CMD,
        json!({
            "provider": report.provider,
            "iterations_run": report.iterations_run,
            "actions_executed": report.actions_executed,
            "actions_rejected": report.actions_rejected,
            "termination_reason": report.termination_reason.as_str(),
            "final_coverage": report.final_coverage,
            "report_ready": report.final_coverage.get("report_ready").cloned().unwrap_or_default(),
            "duration_ms": report.duration_ms,
            "warnings": report.warnings,
        }),
    )
    .with_context(json!({ "session": slug }))
}
