//! Local runtime preflight for `ascent-research` skill/playbooks.

#[cfg(any(
    all(feature = "autoresearch", feature = "provider-claude"),
    all(feature = "autoresearch", feature = "provider-codex")
))]
use crate::autoresearch::provider::{AgentProvider, ProviderError};
use crate::output::Envelope;
use crate::route::rules::load_preset;
use crate::session::layout::research_root;
use serde::Serialize;
use serde_json::json;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const CMD: &str = "research doctor";
const INSTALL_HINT: &str = "cargo install ascent-research --features \"provider-claude provider-codex\" && npm install -g postagent @actionbookdev/cli";

#[derive(Debug, Clone, Serialize)]
struct DoctorCheck {
    name: &'static str,
    ok: bool,
    required: bool,
    detail: String,
}

pub fn run(provider_smoke: bool, tool_smoke: bool, provider: &str) -> Envelope {
    let data_home = research_root();
    let postagent_bin = resolve_bin("POSTAGENT_BIN", "postagent");
    let actionbook_bin = resolve_bin("ACTIONBOOK_BIN", "actionbook");
    let mut checks = vec![
        check_data_home_writable(&data_home),
        check_builtin_preset("tech"),
        check_builtin_preset("sports"),
        check_bin(
            "postagent_bin",
            "POSTAGENT_BIN",
            "postagent",
            true,
            &postagent_bin,
        ),
        check_bin(
            "actionbook_bin",
            "ACTIONBOOK_BIN",
            "actionbook",
            true,
            &actionbook_bin,
        ),
        check_feature("autoresearch_enabled", cfg!(feature = "autoresearch"), true),
        check_feature(
            "provider_claude_enabled",
            cfg!(feature = "provider-claude"),
            false,
        ),
        check_feature(
            "provider_codex_enabled",
            cfg!(feature = "provider-codex"),
            false,
        ),
    ];
    if tool_smoke {
        checks.extend(tool_smoke_checks(&postagent_bin, &actionbook_bin));
    }
    if provider_smoke {
        let providers = match provider {
            "all" => vec!["claude", "codex"],
            "claude" | "codex" => vec![provider],
            other => {
                return Envelope::fail(
                    CMD,
                    "INVALID_PROVIDER",
                    format!("unknown doctor provider '{other}' — expected claude, codex, or all"),
                );
            }
        };
        for provider in providers {
            checks.push(check_provider_smoke(provider));
        }
    }

    let required_failed = checks.iter().any(|check| check.required && !check.ok);
    let payload = json!({
        "status": if required_failed { "missing_required" } else { "ok" },
        "data_home": data_home.display().to_string(),
        "install_hint": INSTALL_HINT,
        "checks": checks,
    });

    if required_failed {
        Envelope::fail(CMD, "DOCTOR_FAILED", "required doctor checks failed").with_details(payload)
    } else {
        Envelope::ok(CMD, payload)
    }
}

fn check_provider_smoke(provider: &str) -> DoctorCheck {
    match provider {
        "claude" => check_claude_smoke(),
        "codex" => check_codex_smoke(),
        _ => DoctorCheck {
            name: "provider_smoke_unknown",
            ok: false,
            required: true,
            detail: format!("unknown provider {provider}"),
        },
    }
}

#[cfg(all(feature = "autoresearch", feature = "provider-claude"))]
fn check_claude_smoke() -> DoctorCheck {
    smoke_provider(
        "provider_claude_smoke",
        crate::autoresearch::claude::ClaudeProvider::new(),
    )
}

#[cfg(not(all(feature = "autoresearch", feature = "provider-claude")))]
fn check_claude_smoke() -> DoctorCheck {
    DoctorCheck {
        name: "provider_claude_smoke",
        ok: false,
        required: true,
        detail: "provider-claude feature not compiled in".to_string(),
    }
}

#[cfg(all(feature = "autoresearch", feature = "provider-codex"))]
fn check_codex_smoke() -> DoctorCheck {
    smoke_provider(
        "provider_codex_smoke",
        crate::autoresearch::codex::CodexProvider::new(),
    )
}

#[cfg(not(all(feature = "autoresearch", feature = "provider-codex")))]
fn check_codex_smoke() -> DoctorCheck {
    DoctorCheck {
        name: "provider_codex_smoke",
        ok: false,
        required: true,
        detail: "provider-codex feature not compiled in".to_string(),
    }
}

#[cfg(any(
    all(feature = "autoresearch", feature = "provider-claude"),
    all(feature = "autoresearch", feature = "provider-codex")
))]
fn smoke_provider<P>(name: &'static str, provider: P) -> DoctorCheck
where
    P: AgentProvider,
{
    let prompt = "Reply with exactly: ok";
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            return DoctorCheck {
                name,
                ok: false,
                required: true,
                detail: format!("runtime init failed: {e}"),
            };
        }
    };
    let result = runtime.block_on(provider.ask(
        "You are a health-check endpoint. Return only the requested literal text.",
        prompt,
    ));
    match result {
        Ok(text) if text.trim().eq_ignore_ascii_case("ok") => DoctorCheck {
            name,
            ok: true,
            required: true,
            detail: "provider returned ok".to_string(),
        },
        Ok(text) => DoctorCheck {
            name,
            ok: false,
            required: true,
            detail: format!(
                "provider returned unexpected text: {}",
                truncate_detail(text.trim())
            ),
        },
        Err(ProviderError::NotAvailable(e)) => DoctorCheck {
            name,
            ok: false,
            required: true,
            detail: format!("provider unavailable: {e}"),
        },
        Err(ProviderError::CallFailed(e)) => DoctorCheck {
            name,
            ok: false,
            required: true,
            detail: format!("provider call failed: {e}"),
        },
        Err(ProviderError::EmptyResponse) => DoctorCheck {
            name,
            ok: false,
            required: true,
            detail: "provider returned empty response".to_string(),
        },
    }
}

fn truncate_detail(text: &str) -> String {
    const MAX: usize = 160;
    if text.len() <= MAX {
        text.to_string()
    } else {
        format!("{}...", &text[..MAX])
    }
}

fn check_data_home_writable(root: &Path) -> DoctorCheck {
    let result = (|| -> Result<(), String> {
        fs::create_dir_all(root).map_err(|e| format!("cannot create data home: {e}"))?;
        let probe = root.join(".doctor-write-test");
        fs::write(&probe, b"ok").map_err(|e| format!("cannot write probe: {e}"))?;
        fs::remove_file(&probe).map_err(|e| format!("cannot remove probe: {e}"))?;
        Ok(())
    })();

    match result {
        Ok(()) => DoctorCheck {
            name: "data_home_writable",
            ok: true,
            required: true,
            detail: format!("writable at {}", root.display()),
        },
        Err(detail) => DoctorCheck {
            name: "data_home_writable",
            ok: false,
            required: true,
            detail,
        },
    }
}

fn check_builtin_preset(name: &'static str) -> DoctorCheck {
    let check_name = match name {
        "tech" => "builtin_preset_tech",
        "sports" => "builtin_preset_sports",
        _ => "builtin_preset_unknown",
    };
    match load_preset(Some(name), None) {
        Ok(preset) => DoctorCheck {
            name: check_name,
            ok: true,
            required: true,
            detail: format!("loaded preset '{}'", preset.name),
        },
        Err(e) => DoctorCheck {
            name: check_name,
            ok: false,
            required: true,
            detail: e.to_string(),
        },
    }
}

fn check_bin(
    name: &'static str,
    env_var: &'static str,
    bin_name: &'static str,
    required: bool,
    resolution: &BinResolution,
) -> DoctorCheck {
    match resolution {
        BinResolution::Found(path) => DoctorCheck {
            name,
            ok: true,
            required,
            detail: format!("found at {}", path.display()),
        },
        BinResolution::MissingEnv(path) => DoctorCheck {
            name,
            ok: false,
            required,
            detail: format!("{env_var} target not found: {}", path.display()),
        },
        BinResolution::MissingPath => DoctorCheck {
            name,
            ok: false,
            required,
            detail: format!("{bin_name} not found on PATH; set {env_var} to override"),
        },
    }
}

fn tool_smoke_checks(postagent: &BinResolution, actionbook: &BinResolution) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    match postagent {
        BinResolution::Found(path) => {
            checks.push(check_command(
                "postagent_version",
                path,
                &["--version"],
                true,
                "postagent --version",
            ));
            checks.push(check_command(
                "postagent_send_help",
                path,
                &["send", "--help"],
                true,
                "postagent send --help",
            ));
            checks.push(check_command(
                "postagent_public_dry_run",
                path,
                &["send", "https://example.com", "--dry-run"],
                false,
                "postagent send https://example.com --dry-run",
            ));
        }
        _ => checks.push(DoctorCheck {
            name: "postagent_version",
            ok: false,
            required: true,
            detail: "postagent binary missing; cannot run tool smoke".to_string(),
        }),
    }

    match actionbook {
        BinResolution::Found(path) => {
            checks.push(check_command(
                "actionbook_version",
                path,
                &["--version"],
                true,
                "actionbook --version",
            ));
            checks.push(check_command(
                "actionbook_browser_list_sessions",
                path,
                &["browser", "list-sessions", "--json"],
                true,
                "actionbook browser list-sessions --json",
            ));
        }
        _ => checks.push(DoctorCheck {
            name: "actionbook_version",
            ok: false,
            required: true,
            detail: "actionbook binary missing; cannot run tool smoke".to_string(),
        }),
    }
    checks
}

fn check_command(
    name: &'static str,
    bin: &Path,
    args: &[&str],
    required: bool,
    label: &str,
) -> DoctorCheck {
    match Command::new(bin).args(args).output() {
        Ok(output) if output.status.success() => DoctorCheck {
            name,
            ok: true,
            required,
            detail: format!("{label} ok: {}", summarize_output(&output.stdout)),
        },
        Ok(output) => {
            let stderr = summarize_output(&output.stderr);
            let stdout = summarize_output(&output.stdout);
            let detail = if stderr.is_empty() {
                format!("{label} exited {}: {stdout}", output.status)
            } else {
                format!("{label} exited {}: {stderr}", output.status)
            };
            DoctorCheck {
                name,
                ok: false,
                required,
                detail,
            }
        }
        Err(e) => DoctorCheck {
            name,
            ok: false,
            required,
            detail: format!("{label} failed to spawn: {e}"),
        },
    }
}

fn summarize_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    truncate_detail(text.trim())
}

fn check_feature(name: &'static str, enabled: bool, required: bool) -> DoctorCheck {
    DoctorCheck {
        name,
        ok: enabled,
        required,
        detail: if enabled {
            "compiled in".to_string()
        } else {
            "not compiled in".to_string()
        },
    }
}

enum BinResolution {
    Found(PathBuf),
    MissingEnv(PathBuf),
    MissingPath,
}

fn resolve_bin(env_var: &str, bin_name: &str) -> BinResolution {
    if let Some(path) = env::var_os(env_var).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(path);
        return if path.is_file() {
            BinResolution::Found(path)
        } else {
            BinResolution::MissingEnv(path)
        };
    }

    let Some(paths) = env::var_os("PATH") else {
        return BinResolution::MissingPath;
    };

    for dir in env::split_paths(&paths) {
        let candidate = dir.join(bin_name);
        if candidate.is_file() {
            return BinResolution::Found(candidate);
        }
    }

    BinResolution::MissingPath
}
