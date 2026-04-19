//! Content smell test. Pure functions (no I/O) — caller supplies parsed
//! subprocess output. Thresholds configurable via env var.

use crate::session::event::RejectReason;

use super::FetchOutcome;

/// Default minimum byte length for an article (readable) browser response.
pub const DEFAULT_ARTICLE_MIN_BYTES: u64 = 500;
/// Default minimum byte length for a short (non-readable) browser response.
pub const DEFAULT_SHORT_MIN_BYTES: u64 = 100;

pub fn article_min_bytes() -> u64 {
    std::env::var("ACTIONBOOK_RESEARCH_SMELL_ARTICLE_MIN_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_ARTICLE_MIN_BYTES)
}

pub fn short_min_bytes() -> u64 {
    std::env::var("ACTIONBOOK_RESEARCH_SMELL_SHORT_MIN_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_SHORT_MIN_BYTES)
}

/// Input describing what a browser subprocess returned.
pub struct BrowserResponse<'a> {
    pub requested_url: &'a str,
    pub observed_url: &'a str,
    pub body_bytes: &'a [u8],
    pub readable_mode: bool,
}

/// Per-call smell-test overrides. Hard gates (about:blank / chrome-error /
/// host mismatch / 0-byte body) are never relaxed by this struct — only
/// the "body is too short" threshold behavior is tunable here.
#[derive(Debug, Clone, Copy, Default)]
pub struct SmellConfig {
    /// When `Some(n)`, overrides both article and short min-byte thresholds
    /// for this call. `None` falls back to the env-driven defaults.
    pub min_bytes_override: Option<u64>,
    /// What to do when the body is shorter than the threshold. Defaults to
    /// reject for backward compatibility.
    pub short_body_mode: ShortBodyMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ShortBodyMode {
    /// Short body is a fatal smell failure (default).
    #[default]
    Reject,
    /// Short body becomes a warning; the source is still accepted and the
    /// raw bytes are still persisted. Useful for SPAs / challenge pages.
    Warn,
}

/// Input describing a postagent response.
pub struct ApiResponse {
    pub status: Option<i32>,
    pub body_non_empty: bool,
    pub body_bytes: u64,
}

/// Evaluate an API response. Rejects on non-2xx status or empty body.
pub fn judge_api(r: &ApiResponse) -> FetchOutcome {
    let mut outcome = FetchOutcome {
        accepted: true,
        observed_url: None,
        observed_bytes: r.body_bytes,
        reject_reason: None,
        warnings: Vec::new(),
        bytes: r.body_bytes,
    };
    if let Some(status) = r.status {
        if !(200..300).contains(&status) {
            outcome.accepted = false;
            outcome.reject_reason = Some(RejectReason::ApiError);
            outcome.warnings.push(format!("http status {status}"));
            return outcome;
        }
    }
    if !r.body_non_empty {
        outcome.accepted = false;
        outcome.reject_reason = Some(RejectReason::EmptyContent);
        outcome.warnings.push("api body empty".into());
    }
    outcome
}

/// Evaluate a browser response with default smell config.
pub fn judge_browser(r: &BrowserResponse) -> FetchOutcome {
    judge_browser_with(r, SmellConfig::default())
}

/// Evaluate a browser response with a per-call smell config. The config can
/// override the short-body threshold and demote "too short" from a fatal
/// rejection to a warning; it **cannot** relax the hard gates (empty
/// observed URL, `about:` / `chrome-error:` schemes, host-level URL
/// mismatch, 0-byte body).
pub fn judge_browser_with(r: &BrowserResponse, cfg: SmellConfig) -> FetchOutcome {
    let body_len = r.body_bytes.len() as u64;
    let mut outcome = FetchOutcome {
        accepted: true,
        observed_url: Some(r.observed_url.to_string()),
        observed_bytes: body_len,
        reject_reason: None,
        warnings: Vec::new(),
        bytes: body_len,
    };

    // ── Hard gate #1: empty / pseudo observed URL ─────────────────────────
    let obs = r.observed_url.trim();
    if obs.is_empty()
        || obs.starts_with("about:")
        || obs.starts_with("chrome-error:")
        || obs == "null"
    {
        outcome.accepted = false;
        outcome.reject_reason = Some(RejectReason::WrongUrl);
        outcome.warnings.push(format!("observed url suspicious: '{obs}'"));
        return outcome;
    }

    // ── Hard gate #2: host-level URL mismatch ─────────────────────────────
    if !urls_compatible(r.requested_url, obs) {
        outcome.accepted = false;
        outcome.reject_reason = Some(RejectReason::WrongUrl);
        outcome.warnings.push(format!(
            "observed url '{obs}' does not match requested '{}'",
            r.requested_url
        ));
        return outcome;
    }

    // ── Hard gate #3: 0-byte body always rejects ──────────────────────────
    if body_len == 0 {
        outcome.accepted = false;
        outcome.reject_reason = Some(RejectReason::EmptyContent);
        outcome.warnings.push("body 0b (no content at all)".into());
        return outcome;
    }

    // ── Soft gate: short body (threshold + mode both tunable) ─────────────
    let min = match cfg.min_bytes_override {
        Some(n) => n,
        None => {
            if r.readable_mode {
                article_min_bytes()
            } else {
                short_min_bytes()
            }
        }
    };
    if body_len < min {
        match cfg.short_body_mode {
            ShortBodyMode::Reject => {
                outcome.accepted = false;
                outcome.reject_reason = Some(RejectReason::EmptyContent);
                outcome.warnings.push(format!(
                    "body {body_len}b below {min}b threshold (readable={})",
                    r.readable_mode
                ));
            }
            ShortBodyMode::Warn => {
                outcome.warnings.push(format!(
                    "short_body_warned: body {body_len}b below {min}b threshold \
                     (accepted via --on-short-body=warn)"
                ));
            }
        }
    }
    outcome
}

/// Normalize two URLs enough to compare: lowercase scheme+host, strip trailing
/// slash, ignore query/fragment. A host-mismatch is a hard fail; path prefix
/// match OK (the page might add query string).
fn urls_compatible(requested: &str, observed: &str) -> bool {
    let (rh, rp) = split_host_path(requested);
    let (oh, op) = split_host_path(observed);
    if rh != oh {
        return false;
    }
    let rp_trim = rp.trim_end_matches('/');
    let op_trim = op.trim_end_matches('/');
    op_trim == rp_trim || op_trim.starts_with(rp_trim)
}

fn split_host_path(url: &str) -> (String, String) {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let no_frag = rest.split('#').next().unwrap_or("");
    let no_query = no_frag.split('?').next().unwrap_or("");
    match no_query.find('/') {
        Some(i) => (no_query[..i].to_ascii_lowercase(), no_query[i..].to_string()),
        None => (no_query.to_ascii_lowercase(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_2xx_non_empty_accepts() {
        let r = ApiResponse {
            status: Some(200),
            body_non_empty: true,
            body_bytes: 1234,
        };
        let o = judge_api(&r);
        assert!(o.accepted);
    }

    #[test]
    fn api_404_rejects() {
        let r = ApiResponse {
            status: Some(404),
            body_non_empty: true,
            body_bytes: 10,
        };
        let o = judge_api(&r);
        assert!(!o.accepted);
        assert_eq!(o.reject_reason, Some(RejectReason::ApiError));
    }

    #[test]
    fn api_empty_body_rejects() {
        let r = ApiResponse {
            status: Some(200),
            body_non_empty: false,
            body_bytes: 0,
        };
        let o = judge_api(&r);
        assert_eq!(o.reject_reason, Some(RejectReason::EmptyContent));
    }

    #[test]
    fn browser_about_blank_rejects() {
        let r = BrowserResponse {
            requested_url: "https://example.com/",
            observed_url: "about:blank",
            body_bytes: b"",
            readable_mode: false,
        };
        let o = judge_browser(&r);
        assert_eq!(o.reject_reason, Some(RejectReason::WrongUrl));
    }

    #[test]
    fn browser_chrome_error_rejects() {
        let r = BrowserResponse {
            requested_url: "https://example.com/",
            observed_url: "chrome-error://chromewebdata/",
            body_bytes: b"",
            readable_mode: false,
        };
        let o = judge_browser(&r);
        assert_eq!(o.reject_reason, Some(RejectReason::WrongUrl));
    }

    #[test]
    fn browser_host_mismatch_rejects() {
        let r = BrowserResponse {
            requested_url: "https://a.com/",
            observed_url: "https://b.com/",
            body_bytes: &[b'x'; 1000],
            readable_mode: true,
        };
        let o = judge_browser(&r);
        assert_eq!(o.reject_reason, Some(RejectReason::WrongUrl));
    }

    #[test]
    fn browser_too_short_rejects() {
        let r = BrowserResponse {
            requested_url: "https://example.com/",
            observed_url: "https://example.com/",
            body_bytes: b"hi",
            readable_mode: true,
        };
        let o = judge_browser(&r);
        assert_eq!(o.reject_reason, Some(RejectReason::EmptyContent));
    }

    #[test]
    fn browser_happy_accepts() {
        let r = BrowserResponse {
            requested_url: "https://example.com/blog",
            observed_url: "https://example.com/blog",
            body_bytes: &vec![b'x'; 800],
            readable_mode: true,
        };
        let o = judge_browser(&r);
        assert!(o.accepted);
    }

    #[test]
    fn browser_trailing_slash_compatible() {
        let r = BrowserResponse {
            requested_url: "https://example.com/blog",
            observed_url: "https://example.com/blog/",
            body_bytes: &vec![b'x'; 800],
            readable_mode: true,
        };
        assert!(judge_browser(&r).accepted);
    }

    #[test]
    fn browser_query_param_after_redirect_ok() {
        let r = BrowserResponse {
            requested_url: "https://example.com/x",
            observed_url: "https://example.com/x/welcome",
            body_bytes: &vec![b'x'; 800],
            readable_mode: true,
        };
        assert!(judge_browser(&r).accepted);
    }

    // ── SmellConfig / ShortBodyMode tests ────────────────────────────────

    #[test]
    fn cli_min_bytes_override_below_default_accepts() {
        let r = BrowserResponse {
            requested_url: "https://example.com/",
            observed_url: "https://example.com/",
            body_bytes: &vec![b'x'; 55],
            readable_mode: false,
        };
        let cfg = SmellConfig {
            min_bytes_override: Some(50),
            ..Default::default()
        };
        assert!(judge_browser_with(&r, cfg).accepted);
    }

    #[test]
    fn warn_mode_keeps_short_body_accepted() {
        let r = BrowserResponse {
            requested_url: "https://example.com/",
            observed_url: "https://example.com/",
            body_bytes: b"hi",
            readable_mode: true,
        };
        let cfg = SmellConfig {
            short_body_mode: ShortBodyMode::Warn,
            ..Default::default()
        };
        let o = judge_browser_with(&r, cfg);
        assert!(o.accepted);
        assert!(o.warnings.iter().any(|w| w.contains("short_body_warned")));
    }

    #[test]
    fn warn_mode_does_not_relax_about_blank() {
        let r = BrowserResponse {
            requested_url: "https://example.com/",
            observed_url: "about:blank",
            body_bytes: b"anything goes here but url is bad",
            readable_mode: false,
        };
        let cfg = SmellConfig {
            short_body_mode: ShortBodyMode::Warn,
            ..Default::default()
        };
        assert_eq!(
            judge_browser_with(&r, cfg).reject_reason,
            Some(RejectReason::WrongUrl)
        );
    }

    #[test]
    fn warn_mode_does_not_relax_host_mismatch() {
        let r = BrowserResponse {
            requested_url: "https://a.com/",
            observed_url: "https://b.com/",
            body_bytes: &vec![b'x'; 1000],
            readable_mode: true,
        };
        let cfg = SmellConfig {
            short_body_mode: ShortBodyMode::Warn,
            ..Default::default()
        };
        assert_eq!(
            judge_browser_with(&r, cfg).reject_reason,
            Some(RejectReason::WrongUrl)
        );
    }

    #[test]
    fn warn_mode_does_not_relax_zero_body() {
        let r = BrowserResponse {
            requested_url: "https://example.com/",
            observed_url: "https://example.com/",
            body_bytes: &[],
            readable_mode: false,
        };
        let cfg = SmellConfig {
            short_body_mode: ShortBodyMode::Warn,
            ..Default::default()
        };
        let o = judge_browser_with(&r, cfg);
        assert!(!o.accepted);
        assert_eq!(o.reject_reason, Some(RejectReason::EmptyContent));
    }

    #[test]
    fn default_config_preserves_legacy_behavior() {
        // body 2 bytes, readable mode on → reject under default
        let r = BrowserResponse {
            requested_url: "https://example.com/",
            observed_url: "https://example.com/",
            body_bytes: b"hi",
            readable_mode: true,
        };
        let default_cfg = SmellConfig::default();
        let o = judge_browser_with(&r, default_cfg);
        assert!(!o.accepted);
        assert_eq!(o.reject_reason, Some(RejectReason::EmptyContent));
    }
}
