//! actionbook browser 3-step subprocess sequence:
//! new-tab → wait network-idle → text [--readable] → close-tab (best-effort).
//!
//! Session and tab IDs are allocated by this module: session = "research-<slug>",
//! tab = "t-<N>" where N is the add sequence number.
//!
//! Each step emits a JSON envelope `{ok, context, data, error, meta}`.
//! We parse the text step's `context.url` + `data.value` for the smell test.

use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::RawFetch;

pub const ACTIONBOOK_STDOUT_CAP: usize = 16 * 1024 * 1024;

pub fn binary() -> String {
    std::env::var("ACTIONBOOK_BIN").unwrap_or_else(|_| "actionbook".to_string())
}

pub struct BrowserRun {
    pub raw: RawFetch,
    pub observed_url: String,
    pub body: Vec<u8>,
}

/// Session name to pass to `actionbook browser`. If the caller has exported
/// `ACTIONBOOK_BROWSER_SESSION`, that value wins and the auto-start step is
/// skipped — useful when the human already has an actionbook session that
/// owns the Chrome profile (actionbook enforces one session per profile, so
/// the CLI cannot create a fresh `research-<slug>` session in parallel).
pub fn session_id_for(slug: &str) -> String {
    match std::env::var("ACTIONBOOK_BROWSER_SESSION") {
        Ok(s) if !s.trim().is_empty() => s,
        _ => format!("research-{slug}"),
    }
}

/// Whether the current run should attempt `browser start` at all. When the
/// caller pins us to an existing session via env, skip auto-start — trying
/// to start a session that already exists is harmless on some actionbook
/// builds and a hard error on others.
pub fn should_autostart_session() -> bool {
    !matches!(
        std::env::var("ACTIONBOOK_BROWSER_SESSION"),
        Ok(s) if !s.trim().is_empty()
    )
}

pub fn tab_id_for(n: u32) -> String {
    format!("t-{n}")
}

/// Run the 3-step sequence. Shared `timeout_ms` budget across all steps.
pub fn run(
    slug: &str,
    tab_n: u32,
    url: &str,
    readable: bool,
    timeout_ms: u64,
) -> Result<BrowserRun, String> {
    let bin = binary();
    let session = session_id_for(slug);
    let tab = tab_id_for(tab_n);
    let start = Instant::now();

    // Make sure session exists. Only auto-start when we own the session name
    // (research-<slug>); if the caller pinned us to an existing session via
    // ACTIONBOOK_BROWSER_SESSION, skip — it already exists by contract.
    if should_autostart_session() {
        let start_res = one_step(
            &bin,
            &["browser", "start", "--session", &session],
            budget_remaining(start, timeout_ms)?,
        );
        if let Ok(r) = start_res {
            // Detect the profile-conflict error, parse out the holder session
            // name, and return a structured hint. Actionbook writes this on
            // **stdout** for plain-text mode — check both channels to be
            // robust across versions.
            if r.exit_code != 0 {
                let both = format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&r.raw_stdout),
                    String::from_utf8_lossy(&r.raw_stderr),
                );
                if let Some(holder) = parse_profile_conflict(&both) {
                    return Err(format!(
                        "browser profile already owned by session '{holder}'; \
                         retry with ACTIONBOOK_BROWSER_SESSION={holder} or close \
                         that session first"
                    ));
                }
                // Any other non-zero exit is treated as benign (session may
                // already be running for an unrelated reason).
            }
        }
    }

    // Step 1: new-tab. When sharing an existing session we cannot predict
    // which tab IDs are free, so let actionbook auto-assign and read the
    // assigned ID out of the envelope. When the session is ours (auto-started
    // above) we keep the deterministic `t-<n>` naming — it's helpful for
    // debugging and matches the raw-file index.
    let sharing = !should_autostart_session();
    let new_tab_args: Vec<&str> = if sharing {
        vec!["browser", "new-tab", url, "--session", &session, "--json"]
    } else {
        vec![
            "browser",
            "new-tab",
            url,
            "--session",
            &session,
            "--tab",
            &tab,
            "--json",
        ]
    };
    let r1 = one_step(&bin, &new_tab_args, budget_remaining(start, timeout_ms)?)?;
    if r1.exit_code != 0 {
        // actionbook writes structured errors as a JSON envelope on stdout
        // when `--json` is set; stderr is usually empty. Read both.
        let stdout_txt = String::from_utf8_lossy(&r1.raw_stdout).into_owned();
        let stderr_txt = String::from_utf8_lossy(&r1.raw_stderr).into_owned();
        let err_msg = extract_json_error(&stdout_txt).unwrap_or_else(|| {
            if !stderr_txt.trim().is_empty() {
                stderr_txt.clone()
            } else {
                stdout_txt.clone()
            }
        });
        return Err(format!(
            "browser new-tab exit {}: {}",
            r1.exit_code, err_msg
        ));
    }

    // If we auto-assigned, parse back the tab ID the daemon picked.
    let tab = if sharing {
        parse_assigned_tab(&r1.raw_stdout).unwrap_or_else(|| tab.clone())
    } else {
        tab
    };

    // Step 2: wait network-idle. Cap wait's portion of the budget so a
    // never-idle page (common on Reddit / SPAs) doesn't starve the text
    // step. 2/3 for wait, ≥ 4s reserved for text.
    let remaining = budget_remaining(start, timeout_ms)?;
    let wait_budget = remaining
        .saturating_sub(4_000)
        .min(remaining * 2 / 3)
        .max(1_000);
    let r2 = one_step(
        &bin,
        &[
            "browser",
            "wait",
            "network-idle",
            "--session",
            &session,
            "--tab",
            &tab,
            "--timeout",
            &wait_budget.to_string(),
            "--json",
        ],
        wait_budget,
    )?;
    // wait-idle timeout is tolerable (per B4 lesson) — don't hard-fail here
    if r2.exit_code != 0 {
        // fall through; text step will validate observed state
    }

    // Step 3: text. `--readable` was removed from actionbook ≥ 1.1.0 — we
    // keep the parameter in this function's signature (upstream callers may
    // still want to signal the intent) but no longer forward it to the
    // subprocess. Readability extraction is performed downstream on the
    // raw text body if needed.
    let _ = readable;
    let arg_refs: Vec<&str> = vec![
        "browser",
        "text",
        "--session",
        &session,
        "--tab",
        &tab,
        "--json",
    ];
    let r3 = one_step(&bin, &arg_refs, budget_remaining(start, timeout_ms)?)?;
    if r3.exit_code != 0 {
        return Err(format!(
            "browser text exit {}; stderr: {}",
            r3.exit_code,
            String::from_utf8_lossy(&r3.raw_stderr)
        ));
    }

    // Best-effort close-tab (ignore failure).
    let _ = one_step(
        &bin,
        &[
            "browser",
            "close-tab",
            "--session",
            &session,
            "--tab",
            &tab,
            "--json",
        ],
        budget_remaining(start, timeout_ms).unwrap_or(2000),
    );

    // Parse text step's JSON envelope for observed_url + body.
    let v: Value = serde_json::from_slice(&r3.raw_stdout).map_err(|e| {
        format!(
            "actionbook browser text returned non-JSON: {e}; first 256 bytes: {}",
            String::from_utf8_lossy(&r3.raw_stdout[..r3.raw_stdout.len().min(256)])
        )
    })?;
    let observed_url = v["context"]["url"].as_str().unwrap_or("").to_string();
    let body = v["data"]["value"]
        .as_str()
        .unwrap_or("")
        .as_bytes()
        .to_vec();

    Ok(BrowserRun {
        raw: r3, // use text step as the "primary" raw (has envelope)
        observed_url,
        body,
    })
}

fn budget_remaining(start: Instant, total_ms: u64) -> Result<u64, String> {
    let elapsed = start.elapsed().as_millis() as u64;
    if elapsed >= total_ms {
        return Err(format!("browser budget exhausted after {elapsed}ms"));
    }
    Ok(total_ms - elapsed)
}

/// Spawn + wait a single actionbook subprocess step.
fn one_step(bin: &str, args: &[&str], timeout_ms: u64) -> Result<RawFetch, String> {
    let start = Instant::now();
    let mut child = Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!(
                "MISSING_DEPENDENCY: actionbook binary '{bin}' not found on PATH (install actionbook or set ACTIONBOOK_BIN)"
            ),
            _ => format!("spawn actionbook: {e}"),
        })?;
    let mut stdout = child.stdout.take().ok_or("no stdout pipe")?;
    let mut stderr = child.stderr.take().ok_or("no stderr pipe")?;
    let deadline = start + Duration::from_millis(timeout_ms);

    let stdout_h = std::thread::spawn(move || {
        let mut buf = Vec::with_capacity(4096);
        let mut tmp = [0u8; 8192];
        loop {
            match stdout.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() + n > ACTIONBOOK_STDOUT_CAP {
                        return Err(buf.len() as u64);
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                Err(_) => break,
            }
        }
        Ok(buf)
    });
    let stderr_h = std::thread::spawn(move || {
        let mut buf = Vec::with_capacity(1024);
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let exit_code = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.code().unwrap_or(-1),
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("actionbook step timed out after {timeout_ms}ms"));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("wait actionbook: {e}")),
        }
    };
    let raw_stdout = stdout_h
        .join()
        .map_err(|_| "stdout thread panicked".to_string())?
        .map_err(|_| {
            format!(
                "actionbook stdout exceeded {} MiB cap",
                ACTIONBOOK_STDOUT_CAP / (1024 * 1024)
            )
        })?;
    let raw_stderr = stderr_h.join().unwrap_or_default();

    Ok(RawFetch {
        raw_stdout,
        raw_stderr,
        exit_code,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

/// Parse the actionbook stderr/stdout for a "profile already in use by session X"
/// error and return X. Works against both plain-text and JSON-envelope output
/// shapes that actionbook currently emits.
fn parse_profile_conflict(text: &str) -> Option<String> {
    // Matches: `profile 'NAME' is already in use by session 'OTHER'`
    let re = regex::Regex::new(r"already in use by session '([^']+)'").ok()?;
    re.captures(text)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

/// Parse the tab ID that actionbook auto-assigned from a `new-tab --json`
/// envelope. Looks at `data.tab.tab_id`, falling back to `data.tab_id` if
/// actionbook ever flattens the shape.
fn parse_assigned_tab(stdout: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(stdout).ok()?;
    v["data"]["tab"]["tab_id"]
        .as_str()
        .or_else(|| v["data"]["tab_id"].as_str())
        .map(str::to_string)
}

/// Pull a human-readable error line out of an actionbook `--json` failure
/// envelope. Falls back to the `code` field if `message` is absent.
fn extract_json_error(stdout: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let err = &v["error"];
    let msg = err["message"].as_str();
    let code = err["code"].as_str();
    match (code, msg) {
        (Some(c), Some(m)) => Some(format!("{c}: {m}")),
        (Some(c), None) => Some(c.to_string()),
        (None, Some(m)) => Some(m.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_env<F: FnOnce()>(key: &str, val: Option<&str>, f: F) {
        let prev = std::env::var(key).ok();
        match val {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
        f();
        match prev {
            Some(p) => unsafe { std::env::set_var(key, p) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    #[test]
    fn session_id_defaults_to_slug_prefix() {
        with_env("ACTIONBOOK_BROWSER_SESSION", None, || {
            assert_eq!(session_id_for("my-slug"), "research-my-slug");
        });
    }

    #[test]
    fn session_id_env_override_wins() {
        with_env("ACTIONBOOK_BROWSER_SESSION", Some("shared-session"), || {
            assert_eq!(session_id_for("my-slug"), "shared-session");
        });
    }

    #[test]
    fn session_id_empty_env_falls_back_to_default() {
        with_env("ACTIONBOOK_BROWSER_SESSION", Some("   "), || {
            assert_eq!(session_id_for("x"), "research-x");
        });
    }

    #[test]
    fn autostart_gated_by_env() {
        with_env("ACTIONBOOK_BROWSER_SESSION", None, || {
            assert!(should_autostart_session());
        });
        with_env("ACTIONBOOK_BROWSER_SESSION", Some("existing"), || {
            assert!(!should_autostart_session());
        });
        with_env("ACTIONBOOK_BROWSER_SESSION", Some(""), || {
            assert!(should_autostart_session());
        });
    }

    #[test]
    fn parse_profile_conflict_plain() {
        let s = "error SESSION_ALREADY_EXISTS: profile 'actionbook' is already in use by session 'research-t1'";
        assert_eq!(parse_profile_conflict(s).as_deref(), Some("research-t1"));
    }

    #[test]
    fn parse_profile_conflict_json_envelope() {
        let s = r#"{"error":{"message":"profile 'actionbook' is already in use by session 'my-sess'"}}"#;
        assert_eq!(parse_profile_conflict(s).as_deref(), Some("my-sess"));
    }

    #[test]
    fn parse_profile_conflict_no_match() {
        assert_eq!(parse_profile_conflict("nothing relevant"), None);
    }

    #[test]
    fn parse_assigned_tab_from_nested_envelope() {
        let s = br#"{"ok":true,"command":"browser new-tab","data":{"tab":{"tab_id":"t-17","title":"","url":"x"}}}"#;
        assert_eq!(parse_assigned_tab(s).as_deref(), Some("t-17"));
    }

    #[test]
    fn parse_assigned_tab_from_flat_envelope() {
        let s = br#"{"ok":true,"data":{"tab_id":"t-99"}}"#;
        assert_eq!(parse_assigned_tab(s).as_deref(), Some("t-99"));
    }

    #[test]
    fn parse_assigned_tab_missing() {
        assert_eq!(parse_assigned_tab(b"{}"), None);
        assert_eq!(parse_assigned_tab(b"not json"), None);
    }

    #[test]
    fn extract_json_error_tab_conflict() {
        let s = r#"{"ok":false,"error":{"code":"TAB_ID_CONFLICT","message":"tab ID 't-1' already exists in this session"}}"#;
        let msg = extract_json_error(s).unwrap();
        assert!(msg.contains("TAB_ID_CONFLICT"));
        assert!(msg.contains("already exists"));
    }

    #[test]
    fn extract_json_error_falls_back_to_code_only() {
        let s = r#"{"ok":false,"error":{"code":"SOMETHING"}}"#;
        assert_eq!(extract_json_error(s).as_deref(), Some("SOMETHING"));
    }

    #[test]
    fn extract_json_error_missing_returns_none() {
        assert_eq!(extract_json_error("{}"), None);
        assert_eq!(extract_json_error("not json"), None);
    }
}
