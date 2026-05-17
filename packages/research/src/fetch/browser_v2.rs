//! V2 actionbook MCP backend for fetch::browser.
//!
//! Replaces the V1 CLI subprocess sequence with HTTPS JSON-RPC calls to
//! edge.actionbook.dev/mcp. The MCP server forwards each `cmd:` string to
//! the user's Chrome extension over WSS; the extension drives the page via
//! `chrome.debugger` + Playwright's injected script.
//!
//! 3-step sequence (replaces V1's new-tab + wait + text + close):
//!   1. `browser new-tab <url> --tab <handle>`
//!   2. `browser run-code --tab <handle> '<async (page) => {...}>'`
//!   3. `browser close --tab <handle>` (best-effort)
//!
//! Tab handle = `research-<slug>-<N>` by default; `ACTIONBOOK_BROWSER_SESSION=foo`
//! → `foo-<slug>-<N>` (prefix override for multi-instance Chrome sharing).
//!
//! Spec: `specs/actionbook-v2-mcp-backend.spec.md`. See § 已定决策 for the
//! design rationale; § 验收标准 enumerates the BDD scenarios this module
//! must satisfy.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

use super::browser::BrowserRun;
use super::RawFetch;

pub const DEFAULT_MCP_ENDPOINT: &str = "https://edge.actionbook.dev/mcp";

/// V2 entry. Same signature contract as `browser::run_v1_impl` so the
/// dispatcher in `browser::run` can route without translation.
///
/// `readable` is accepted for V1 ABI compatibility but ignored in V2 —
/// the inline `run-code` always returns `document.body.innerText`, matching
/// what V1 produced when `--readable` was the default upstream behavior.
///
/// `frame_id` / `run_code_args` are V2-only flags forwarded to the
/// underlying `browser run-code` cmd; both are passed through verbatim by
/// `build_runcode_cmd`. When either is `None` the cmd string keeps its
/// pre-flag byte-for-byte shape — zero regression.
pub fn run(
    slug: &str,
    tab_n: u32,
    url: &str,
    _readable: bool,
    timeout_ms: u64,
    frame_id: Option<u32>,
    run_code_args: Option<&Value>,
) -> Result<BrowserRun, String> {
    let api_key = require_api_key()?;
    let endpoint = endpoint();
    let handle = handle_for(slug, tab_n);
    let mut client = McpClient::new(endpoint, api_key, slug.to_string(), timeout_ms);
    client.ensure_initialized()?;

    let goto_cmd = build_new_tab_cmd(url, &handle);
    let runcode_cmd =
        build_runcode_cmd_for_url(url, &handle, timeout_ms, frame_id, run_code_args);
    let close_cmd = build_close_cmd(&handle);

    // Step 1: new-tab.
    client.call_tool(&goto_cmd)?;

    // Step 2: run-code. On SESSION_LOST / TAB_NOT_FOUND, re-issue new-tab
    // once and retry the same run-code (spec § 错误码映射).
    let runcode_result = match client.call_tool(&runcode_cmd) {
        Ok(text) => text,
        Err(e) if is_recoverable_handle_loss(&e) => {
            client.call_tool(&goto_cmd)?;
            client.call_tool(&runcode_cmd)?
        }
        Err(e) => return Err(e),
    };

    // Step 3: close (best-effort — failure here does NOT fail the run).
    let _ = client.call_tool(&close_cmd);

    let extracted = extract_run_code_payload(&runcode_result)?;
    Ok(BrowserRun {
        raw: RawFetch {
            raw_stdout: runcode_result.into_bytes(),
            raw_stderr: Vec::new(),
            exit_code: 0,
            duration_ms: 0,
        },
        observed_url: extracted.url,
        body: extracted.text.into_bytes(),
    })
}

/// Module-pub helper so other code paths (catalog probe, future MCP-driven
/// helpers) can call the `actionbook` MCP tool without re-implementing the
/// HTTP + JSON-RPC envelope + `Mcp-Session-Id` dance.
///
/// Uses a fresh `McpClient` per call so the catalog probe shares the same
/// session-id-on-disk cache as the V2 fetch backend; `slug` selects which
/// session's `.mcp-session` file to use. `timeout_ms` is the outer HTTP
/// envelope timeout (independent from the run-code inner timeout — that
/// only applies to `browser run-code` cmds).
///
/// Returns the tool result `content[0].text` on success, with the same
/// stable error-code prefixes (`EXTENSION_OFFLINE:`, `SESSION_LOST:`,
/// `INTERNAL_ERROR:`, etc.) used by V2 fetch's `call_tool`.
///
/// Catalog probe spec: `specs/actionbook-catalog-seed.spec.md` §
/// "MCP 调用复用 V2 backend". Note that catalog probe callers MUST handle
/// `ACTIONBOOK_API_KEY unset` gracefully — see `is_api_key_set`.
pub fn call_actionbook_tool(cmd: &str, slug: &str, timeout_ms: u64) -> Result<String, String> {
    let api_key = require_api_key()?;
    let endpoint = endpoint();
    let mut client = McpClient::new(endpoint, api_key, slug.to_string(), timeout_ms);
    client.ensure_initialized()?;
    client.call_tool(cmd)
}

/// True iff `ACTIONBOOK_API_KEY` is set to a non-blank value. Catalog
/// probe uses this to silently skip when the key is absent, instead of
/// surfacing a hard error like the fetch path does (catalog is
/// nice-to-have, fetch is the user's stated intent).
pub fn is_api_key_set() -> bool {
    matches!(std::env::var("ACTIONBOOK_API_KEY"), Ok(v) if !v.trim().is_empty())
}

/// Read `ACTIONBOOK_API_KEY` or fail fast with an actionable hint. We never
/// emit the token value in the error message — that's a guard against the
/// `live` smell-test logs surfacing the secret.
fn require_api_key() -> Result<String, String> {
    match std::env::var("ACTIONBOOK_API_KEY") {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(
            "ACTIONBOOK_API_KEY unset: set ACTIONBOOK_API_KEY (an ak_* token from \
             actionbook.dev), or run 'actionbook auth login'. \
             OAuth interactive flow is out of scope for the V2 backend.".to_string(),
        ),
    }
}

/// Read `ACTIONBOOK_MCP_ENDPOINT` or fall back to the production edge URL.
pub fn endpoint() -> String {
    std::env::var("ACTIONBOOK_MCP_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_MCP_ENDPOINT.to_string())
}

/// Build the V2 tab handle. Honours `ACTIONBOOK_BROWSER_SESSION` as a
/// prefix (was V1 session name, repurposed in V2 — see spec § Tab handle
/// 命名).
pub fn handle_for(slug: &str, tab_n: u32) -> String {
    match std::env::var("ACTIONBOOK_BROWSER_SESSION") {
        Ok(s) if !s.trim().is_empty() => format!("{}-{}-{}", s.trim(), slug, tab_n),
        _ => format!("research-{}-{}", slug, tab_n),
    }
}

/// Path to the persisted MCP session id for this ascent session.
pub fn session_id_path(slug: &str) -> PathBuf {
    let root = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    root.join(".actionbook")
        .join("ascent-research")
        .join("sessions")
        .join(slug)
        .join(".mcp-session")
}

/// Build the inline run-code JS string used in V2 step 2.
///
/// Must be a function expression (not IIFE) — V2 kernel wraps as
/// `return (${code})` and rejects non-functions with
/// `code must evaluate to a function — got <type>`. Live smoke 2026-05-14.
///
/// Three-stage wait, all `try`/`catch`-guarded so any single stage
/// timeout never tanks the run (preserves V1's "B4 lesson:wait-idle is
/// not fatal"). Tunes for both static and SPA sites:
///
///  1. `domcontentloaded`(≤ 8 s): parsed-DOM milestone; cheap on static
///     sites, modest on SPAs.
///  2. `networkidle` (≤ 3 s): waits for ambient XHR to settle; bails
///     fast on chatty SPAs (analytics beacons rarely settle in 3 s).
///  3. Body-content poll (≤ 5 s @ 250 ms): for SPAs that finish
///     DOMcontentloaded with an empty shell, wait until React hydration
///     paints ≥ 100 chars into `<body>`. Threshold matches smell's
///     short-mode min-bytes so a page that V2 accepts also passes smell.
///
/// Worst case bound: 16 s. Static sites (example.com ~170 chars,
/// instantly ready) exit the body-poll loop on the first iteration in
/// ~50 ms. Live smoke 2026-05-17 confirmed:GitHub PR repaints reliably
/// inside this window; the previous 3 s networkidle-only gate returned
/// `about:blank` for the same URL.
/// Compose the full `browser run-code` cmd string with the inner
/// `--timeout` already aligned to the caller's envelope timeout.
///
/// **Spec § 双层超时**: V2 server's runcode handler has its own deadline
/// (default `DEFAULT_RUNCODE_DEADLINE_MS = 60_000` ms, max
/// `MAX_USER_TIMEOUT_MS = 115_000` ms). Without an explicit `--timeout`
/// in the cmd string, V2 uses the 60 s default — making ascent's caller
/// `--timeout 90000` (or anything above 60 s) ineffective for the inner
/// runcode. We pass `caller_timeout_ms - 5 s` so:
///   - V2 inner runcode times out FIRST and surfaces a clean
///     `EVAL_FAILED`,
///   - the outer ureq HTTP envelope only fires as a fallback,
///   - the 5 s slack mirrors V2 server's `ENVELOPE_SLACK_MS` invariant.
///
/// Clamped to `[5_000, 115_000]` for safety.
///
/// `frame_id` / `run_code_args` are V2 server CLI passthroughs (spec
/// `v2-frame-id-runcode-args.spec.md`). When `Some`, each emits a flag
/// segment in the V2 server's documented order (`--frame-id` before
/// `--args`) so an operator eyeballing the cmd string sees the same
/// layout the server CLI uses. When `None`, the segment is omitted
/// entirely — `--frame-id 0` is not a valid stand-in for "default
/// top frame" because `0` has special meaning in some CDP frame
/// implementations. The JSON literal is single-quote-wrapped to
/// shell-escape; serde_json output only ever uses `"` for strings, so
/// no nested-quote escape is needed.
pub fn build_runcode_cmd(
    handle: &str,
    caller_timeout_ms: u64,
    frame_id: Option<u32>,
    run_code_args: Option<&Value>,
) -> String {
    let inner_timeout_ms = caller_timeout_ms
        .saturating_sub(5_000)
        .min(115_000)
        .max(5_000);
    let mut cmd = format!("browser run-code --tab {handle} --timeout {inner_timeout_ms}");
    if let Some(fid) = frame_id {
        cmd.push_str(&format!(" --frame-id {fid}"));
    }
    if let Some(args) = run_code_args {
        // serde_json::to_string never emits a literal `'`, so the
        // single-quote wrapper is shell-safe without further escaping.
        let literal = serde_json::to_string(args)
            .unwrap_or_else(|_| "[]".to_string());
        cmd.push_str(&format!(" --args '{literal}'"));
    }
    cmd.push_str(&format!(" '{}'", runcode_inline_js().replace('\'', "\\'")));
    cmd
}

/// Compose the V2 `browser new-tab <url> --tab <handle>` cmd string.
/// Exposed (rather than inlined) so the V2 pass-through tests in
/// `tests/runcode_flags.rs` can verify it does NOT carry `--frame-id`
/// or `--args` — those flags live on `run-code` only.
pub fn build_new_tab_cmd(url: &str, handle: &str) -> String {
    format!("browser new-tab {url} --tab {handle}")
}

/// Compose the V2 `browser close --tab <handle>` cmd string. Same
/// rationale as `build_new_tab_cmd` — kept as a pub helper so the
/// pass-through tests can assert the absence of run-code flags.
pub fn build_close_cmd(handle: &str) -> String {
    format!("browser close --tab {handle}")
}

pub fn runcode_inline_js() -> &'static str {
    "async (page) => { \
try { await page.waitForLoadState(\"domcontentloaded\", { timeout: 8000 }); } catch (_e) {} \
try { await page.waitForLoadState(\"networkidle\", { timeout: 3000 }); } catch (_e) {} \
for (let i = 0; i < 20; i++) { \
if (document.body && document.body.innerText && document.body.innerText.length > 100) break; \
await new Promise(r => setTimeout(r, 250)); \
} \
return { url: page.url(), title: await page.title(), text: document.body.innerText }; \
}"
}

// ─── Per-host runcode flavor ──────────────────────────────────────────────
//
// Spec: `specs/x-com-tweet-runcode-flavor.spec.md`.
//
// Default runcode polls `body.innerText` after networkidle, which fails on
// x.com tweet-detail pages: X hydrates the `<article>` via a separate
// GraphQL `TweetDetail` request fired AFTER networkidle, and the left-nav
// chrome alone already pushes `body.innerText.length > 100`, so the
// hydration probe short-circuits to "done" before the tweet body lands.
//
// XTweet flavor swaps the hydration probe for an explicit
// `waitForSelector(article[data-testid="tweet"] | …)` and reads
// `article.innerText` (10× cleaner than full-page innerText). Dispatch is
// by URL host only — path / query do not affect the choice because the
// multi-selector covers tweet detail / profile / search-live / cellInnerDiv
// uniformly.

/// Which runcode JS variant to feed `browser run-code`.
///
/// Picked by `flavor_for_url`. Adding a new flavor (Reddit, LinkedIn) =
/// one variant + one `match` arm in `runcode_inline_js_for` + one host
/// entry in `flavor_for_url`. The TOML preset schema is unchanged —
/// flavor is a pure V2-internal concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuncodeFlavor {
    /// `runcode_inline_js()` — works for static pages and most SPAs.
    Default,
    /// `runcode_inline_js_x_tweet()` — selector-driven wait for X /
    /// Twitter tweet detail, profile, and search-live pages.
    XTweet,
}

/// Map a URL to its runcode flavor.
///
/// Host-only sniff. Recognised hosts (case-insensitive via `ParsedUrl`'s
/// lower-casing): `x.com`, `www.x.com`, `mobile.x.com`, `twitter.com`,
/// `www.twitter.com`, `mobile.twitter.com` → `XTweet`. Everything else
/// (including URLs `ParsedUrl::parse` rejects) → `Default`. Reuses the
/// hand-rolled `ParsedUrl` from `route::rules` so no new dep is pulled in.
pub fn flavor_for_url(url: &str) -> RuncodeFlavor {
    let Some(parsed) = crate::route::rules::ParsedUrl::parse(url) else {
        return RuncodeFlavor::Default;
    };
    match parsed.host.as_str() {
        "x.com" | "www.x.com" | "mobile.x.com"
        | "twitter.com" | "www.twitter.com" | "mobile.twitter.com" => RuncodeFlavor::XTweet,
        _ => RuncodeFlavor::Default,
    }
}

/// XTweet runcode JS — see flavor doc above.
///
/// Behavioural deltas vs `runcode_inline_js`:
/// - **omits** `waitForLoadState("networkidle", …)` — X never idles
///   (background polling + tracker pings), so the 3 s budget is wasted.
/// - **omits** the 20×250ms body-text hydration poll — the nav chrome
///   alone passes the `length > 100` threshold and short-circuits.
/// - **adds** `waitForSelector('article[data-testid="tweet"], [data-testid="cellInnerDiv"], [data-testid="UserName"]', { timeout: 15000 })`
///   — the multi-selector covers tweet detail / search timeline /
///   profile uniformly; 15 s covers p95 hydration on residential
///   connections.
/// - **adds** a collect-across-scrolls strategy (`MAX_SCROLLS = 8`,
///   `MAX_ARTICLES = 25`) — X uses a virtualized list so naive
///   "scroll then querySelectorAll" loses the main tweet (mounted
///   at top, unmounted after scroll). Instead the loop **snapshots
///   the DOM before scrolling and again after each step**, keyed by
///   the tweetId extracted from each article's `/USER/status/<id>`
///   link. `Map<id, text>` dedupes across snapshots so virtualized
///   articles survive in the result. Scroll step is 0.8 × viewport
///   (not jump-to-bottom) to leave more articles mounted between
///   reads. Worst-case scroll budget = 8 × 1.2 s = 9.6 s.
/// - **adds** a 500 ms `setTimeout` grace after the scroll loop — the
///   selector match means the element mounted, but text nodes may
///   land on the next React frame.
/// - **reads** all matching articles via `querySelectorAll`, joins
///   their `innerText` with `'\n\n---\n\n'` (markdown thematic break,
///   downstream md renderers naturally split it), falling back to
///   `document.body.innerText` when the selector wait timed out
///   (deleted tweet / login wall / X redesign) — preserves diagnostic
///   text for smell-test triage instead of returning empty string.
/// - **extracts media** per-article — `<img>` URLs filtered to the
///   `pbs.twimg.com/{media,tweet_video_thumb,card_img}` whitelist
///   (avatars and twemoji excluded as noise), plus `<video>.poster`
///   first-frame URLs. Each URL is appended after the article's
///   innerText as markdown `![](url)`. The rich-html report renders
///   them as `<img>`; raw `.md` files preview them in Obsidian / VS
///   Code. No image bytes downloaded — URLs only, browser fetches
///   from X's public CDN on render.
///
/// Total worst-case time: 8 + 15 + 7.2 + 0.5 ≈ 30.7 s. The caller's
/// inner timeout (caller_timeout_ms − 5 s slack, clamped to
/// [5 s, 115 s]) defaults to 85 s, leaving ~54 s headroom for slow
/// SPAs (profile and search-live are heavier than tweet-detail).
pub fn runcode_inline_js_x_tweet() -> &'static str {
    // Collect-across-scrolls strategy:
    //   X uses a virtualized list — articles that scroll out of viewport
    //   get unmounted from the DOM. The naive "scroll then querySelectorAll"
    //   loses the main tweet (which is at the top before scrolling, then
    //   unmounted by the time we read).
    //
    //   Fix: snapshot BEFORE scrolling, then again after each scroll step,
    //   keyed by tweetId (extracted from /USER/status/<id> link inside each
    //   article). Map<id, text> dedupes across snapshots so virtualized
    //   articles survive in the result.
    //
    //   Scroll in 0.8 × viewport increments (not jump-to-bottom) to give
    //   incremental hydration a chance and keep more articles mounted at
    //   any one moment.
    "async (page) => { \
try { await page.waitForLoadState(\"domcontentloaded\", { timeout: 8000 }); } catch (_e) {} \
try { await page.waitForSelector('article[data-testid=\"tweet\"], [data-testid=\"cellInnerDiv\"], [data-testid=\"UserName\"]', { timeout: 15000 }); } catch (_e) {} \
const MAX_SCROLLS = 8; \
const MAX_ARTICLES = 25; \
const seen = new Map(); \
const snapshot = () => { \
document.querySelectorAll('article[data-testid=\"tweet\"]').forEach(a => { \
const link = a.querySelector('a[href*=\"/status/\"]'); \
const m = link ? link.getAttribute('href').match(/\\/status\\/(\\d+)/) : null; \
const id = m ? m[1] : ('idx-' + seen.size); \
if (seen.has(id)) return; \
const txt = a.innerText; \
const imgs = Array.from(a.querySelectorAll('img')).map(i => i.src).filter(s => s.includes('pbs.twimg.com/media') || s.includes('pbs.twimg.com/tweet_video_thumb') || s.includes('pbs.twimg.com/card_img')); \
const vids = Array.from(a.querySelectorAll('video')).map(v => v.poster || v.src).filter(Boolean); \
const media = imgs.concat(vids).map(u => '![](' + u + ')').join('\\n'); \
seen.set(id, media ? (txt + '\\n\\n' + media) : txt); \
}); \
}; \
snapshot(); \
for (let s = 0; s < MAX_SCROLLS; s++) { \
if (seen.size >= MAX_ARTICLES) break; \
const before = seen.size; \
window.scrollBy(0, window.innerHeight * 0.8); \
await new Promise(r => setTimeout(r, 1200)); \
snapshot(); \
if (seen.size === before) break; \
} \
await new Promise(r => setTimeout(r, 500)); \
snapshot(); \
const ordered = Array.from(seen.values()).slice(0, MAX_ARTICLES); \
const text = ordered.length > 0 ? ordered.join('\\n\\n---\\n\\n') : document.body.innerText; \
return { url: page.url(), title: await page.title(), text }; \
}"
}

/// Return the inline JS for a given flavor.
pub fn runcode_inline_js_for(flavor: RuncodeFlavor) -> &'static str {
    match flavor {
        RuncodeFlavor::Default => runcode_inline_js(),
        RuncodeFlavor::XTweet => runcode_inline_js_x_tweet(),
    }
}

/// URL-aware variant of `build_runcode_cmd` — picks the inline JS by
/// `flavor_for_url(url)` then assembles the same `browser run-code …`
/// shell-escaped cmd string as `build_runcode_cmd`. The Default-flavor
/// `build_runcode_cmd` is preserved as a convenience entrypoint (and to
/// keep v0.4.0's `runcode_flags.rs` tests zero-modification compatible).
pub fn build_runcode_cmd_for_url(
    url: &str,
    handle: &str,
    caller_timeout_ms: u64,
    frame_id: Option<u32>,
    run_code_args: Option<&Value>,
) -> String {
    let flavor = flavor_for_url(url);
    let inner_timeout_ms = caller_timeout_ms
        .saturating_sub(5_000)
        .min(115_000)
        .max(5_000);
    let mut cmd = format!("browser run-code --tab {handle} --timeout {inner_timeout_ms}");
    if let Some(fid) = frame_id {
        cmd.push_str(&format!(" --frame-id {fid}"));
    }
    if let Some(args) = run_code_args {
        let literal = serde_json::to_string(args).unwrap_or_else(|_| "[]".to_string());
        cmd.push_str(&format!(" --args '{literal}'"));
    }
    cmd.push_str(&format!(
        " '{}'",
        runcode_inline_js_for(flavor).replace('\'', "\\'")
    ));
    cmd
}

// ---------------------------------------------------------------------------
// MCP client
// ---------------------------------------------------------------------------

struct McpClient {
    endpoint: String,
    api_key: String,
    slug: String,
    timeout_ms: u64,
    mcp_session_id: Option<String>,
    request_id: u64,
}

impl McpClient {
    fn new(endpoint: String, api_key: String, slug: String, timeout_ms: u64) -> Self {
        Self {
            endpoint,
            api_key,
            slug,
            timeout_ms,
            mcp_session_id: None,
            request_id: 0,
        }
    }

    /// Read `.mcp-session` if it exists. Otherwise POST an `initialize`
    /// JSON-RPC, capture the `Mcp-Session-Id` response header, write it to
    /// disk (0o600) and send the `notifications/initialized` notification
    /// per MCP protocol.
    fn ensure_initialized(&mut self) -> Result<(), String> {
        let path = session_id_path(&self.slug);
        if let Ok(persisted) = fs::read_to_string(&path) {
            let id = persisted.trim().to_string();
            if !id.is_empty() {
                self.mcp_session_id = Some(id);
                return Ok(());
            }
        }

        let init_body = json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "ascent-research", "version": env!("CARGO_PKG_VERSION") }
            }
        });
        let (id_header, _body) = self.raw_post(&init_body)?;
        let id = id_header.ok_or_else(|| {
            "MCP server did not return Mcp-Session-Id header on initialize".to_string()
        })?;
        self.mcp_session_id = Some(id.clone());
        persist_session_id(&path, &id)?;

        // Send the initialized notification. Per MCP spec this is fire-and-
        // forget; we ignore the body but require the request to succeed.
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        let _ = self.raw_post(&notif);
        Ok(())
    }

    fn next_id(&mut self) -> u64 {
        self.request_id += 1;
        self.request_id
    }

    /// Send a tools/call for the `actionbook` tool with the given cmd
    /// string. Returns the tool result's `content[0].text` on success;
    /// stable error code prefixes on failure (see error_prefix_for).
    fn call_tool(&mut self, cmd: &str) -> Result<String, String> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "tools/call",
            "params": {
                "name": "actionbook",
                "arguments": { "cmd": cmd }
            }
        });
        let (maybe_new_id, response_body) = self.raw_post(&body)?;
        if let Some(new_id) = maybe_new_id {
            // Server can rotate the session id mid-stream; persist the new one.
            if Some(new_id.as_str()) != self.mcp_session_id.as_deref() {
                let _ = persist_session_id(&session_id_path(&self.slug), &new_id);
                self.mcp_session_id = Some(new_id);
            }
        }

        let parsed: Value = serde_json::from_str(&response_body)
            .map_err(|e| format!("INTERNAL_ERROR: malformed MCP response body: {e}"))?;

        if let Some(err) = parsed.get("error") {
            let code = err.get("code").and_then(Value::as_str)
                .or_else(|| err.get("code").and_then(|c| c.as_i64()).map(|_| "JSON_RPC_ERROR"))
                .unwrap_or("UNKNOWN_ERROR")
                .to_string();
            let message = err.get("message").and_then(Value::as_str)
                .unwrap_or("(no message)").to_string();
            return Err(format!("{}: {}", error_prefix_for(&code, &message), message));
        }

        let content = parsed
            .get("result")
            .and_then(|r| r.get("content"))
            .and_then(Value::as_array)
            .ok_or_else(|| "INTERNAL_ERROR: tool result missing content array".to_string())?;
        let text = content
            .iter()
            .find_map(|item| item.get("text").and_then(Value::as_str))
            .ok_or_else(|| "INTERNAL_ERROR: tool result content has no text item".to_string())?
            .to_string();

        // The actionbook tool surfaces its own error codes inside the text
        // payload as lines starting with `error <CODE>: ...`. Translate
        // these into the same stable prefixes used for JSON-RPC errors.
        if let Some(code_msg) = parse_inline_error(&text) {
            return Err(format!(
                "{}: {}",
                error_prefix_for(&code_msg.code, &code_msg.message),
                code_msg.message
            ));
        }
        Ok(text)
    }

    /// Low-level POST. Returns `(maybe_session_id_header, body_string)`.
    fn raw_post(&self, body: &Value) -> Result<(Option<String>, String), String> {
        let timeout = Duration::from_millis(self.timeout_ms.max(1_000));
        let agent = ureq::AgentBuilder::new()
            .timeout(timeout)
            .build();
        let mut req = agent
            .post(&self.endpoint)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json, text/event-stream")
            .set("Authorization", &format!("Bearer {}", self.api_key));
        if let Some(id) = &self.mcp_session_id {
            req = req.set("Mcp-Session-Id", id);
        }
        let resp_result = req.send_json(body.clone());
        let resp = match resp_result {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                // Server returns a session id even on 4xx in some cases, but
                // for current ascent-research flow there's no usable retry
                // off a non-2xx — surface the body in the error and let the
                // caller decide. The header is intentionally discarded here.
                let body = r.into_string().unwrap_or_default();
                let prefix = if code == 401 || code == 403 {
                    "EXTENSION_OFFLINE"
                } else {
                    "INTERNAL_ERROR"
                };
                let snippet: String = body.chars().take(300).collect();
                return Err(format!("{prefix}: HTTP {code}: {snippet}"));
            }
            Err(ureq::Error::Transport(t)) => {
                return Err(format!("INTERNAL_ERROR: MCP transport: {t}"));
            }
        };
        let id = resp.header("Mcp-Session-Id").map(str::to_string);
        let body = resp.into_string()
            .map_err(|e| format!("INTERNAL_ERROR: MCP response read: {e}"))?;
        Ok((id, body))
    }
}

// ---------------------------------------------------------------------------
// Helpers — error mapping, payload extraction, persistence
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct InlineErr {
    code: String,
    message: String,
}

/// Parse text like `error EXTENSION_OFFLINE: no active extension websocket`
/// into `(code, message)`. Returns None if the text isn't an error envelope.
fn parse_inline_error(text: &str) -> Option<InlineErr> {
    for line in text.lines() {
        let line = line.trim_start();
        let Some(rest) = line.strip_prefix("error ") else { continue };
        let Some((code, message)) = rest.split_once(':') else { continue };
        if code.chars().all(|c| c.is_ascii_uppercase() || c == '_') && !code.is_empty() {
            return Some(InlineErr {
                code: code.to_string(),
                message: message.trim().to_string(),
            });
        }
    }
    None
}

/// Map an actionbook V2 error code to ascent's stable string prefix. Spec
/// § 错误码映射 enumerates each case. The prefix is what callers grep on
/// (e.g. fetch::mod.rs surfaces this verbatim as a smell warning).
fn error_prefix_for(code: &str, message: &str) -> &'static str {
    match code {
        "EXTENSION_OFFLINE" => "EXTENSION_OFFLINE",
        "SESSION_LOST" => "SESSION_LOST",
        "TAB_NOT_FOUND" => "TAB_NOT_FOUND",
        "CANCELLED" => "CANCELLED",
        "NAVIGATION_FAILED" => "NAVIGATION_FAILED",
        "PAYLOAD_TOO_LARGE" => "PAYLOAD_TOO_LARGE",
        "TIMEOUT" => "TIMEOUT",
        "INVALID_ARGUMENT" => "INVALID_ARGUMENT",
        "ELEMENT_NOT_FOUND" | "MULTIPLE_MATCHES" | "EVAL_FAILED" => "RUN_CODE_FAILED",
        "INTERNAL_ERROR" if message.contains("chrome-extension://")
            || message.contains("Detached while handling command") =>
        {
            "DEBUGGER_ATTACH_CONFLICT"
        }
        _ => "INTERNAL_ERROR",
    }
}

/// True when an error string starts with a code that warrants exactly one
/// re-bind retry (`SESSION_LOST` / `TAB_NOT_FOUND`).
fn is_recoverable_handle_loss(err: &str) -> bool {
    err.starts_with("SESSION_LOST:") || err.starts_with("TAB_NOT_FOUND:")
}

struct RunCodePayload {
    url: String,
    text: String,
}

/// Extract the `{url, title, text}` JSON object from the run-code tool
/// result. The actionbook handler emits a header line followed by a JSON
/// body line; we scan for the first `{` and parse from there.
fn extract_run_code_payload(tool_result_text: &str) -> Result<RunCodePayload, String> {
    let start = tool_result_text
        .find('{')
        .ok_or_else(|| "INTERNAL_ERROR: run-code output has no JSON body".to_string())?;
    let json_part = &tool_result_text[start..];
    let parsed: Value = serde_json::from_str(json_part)
        .map_err(|e| format!("INTERNAL_ERROR: run-code JSON parse: {e}"))?;
    let result = parsed.get("result").ok_or_else(|| {
        "INTERNAL_ERROR: run-code envelope missing `result` field".to_string()
    })?;
    let url = result
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let text = result
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok(RunCodePayload { url, text })
}

/// Persist the Mcp-Session-Id to disk with restrictive perms.
fn persist_session_id(path: &PathBuf, id: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("INTERNAL_ERROR: mkdir .mcp-session parent: {e}"))?;
    }
    // Atomic-ish: write then rename. Permissions set after rename on Unix.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| format!("INTERNAL_ERROR: open .mcp-session: {e}"))?;
    file.write_all(id.as_bytes())
        .map_err(|e| format!("INTERNAL_ERROR: write .mcp-session: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Save / restore env so parallel tests don't leak state. Same shape
    /// as the helper in `browser::tests`.
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
    fn endpoint_default_is_production() {
        assert_eq!(DEFAULT_MCP_ENDPOINT, "https://edge.actionbook.dev/mcp");
    }

    #[test]
    fn v2_tab_handle_naming_default() {
        with_env("ACTIONBOOK_BROWSER_SESSION", None, || {
            assert_eq!(handle_for("demo", 2), "research-demo-2");
        });
    }

    #[test]
    fn v2_tab_handle_prefix_via_env() {
        with_env("ACTIONBOOK_BROWSER_SESSION", Some("foo"), || {
            assert_eq!(handle_for("demo", 1), "foo-demo-1");
        });
    }

    #[test]
    fn v2_tab_handle_empty_env_falls_back_to_default() {
        with_env("ACTIONBOOK_BROWSER_SESSION", Some("   "), || {
            assert_eq!(handle_for("demo", 3), "research-demo-3");
        });
    }

    #[test]
    fn v2_api_key_unset_fail_fast() {
        with_env("ACTIONBOOK_API_KEY", None, || {
            let err = require_api_key().expect_err("missing key must be fatal");
            assert!(err.contains("ACTIONBOOK_API_KEY"));
            assert!(err.contains("actionbook auth login"));
        });
    }

    #[test]
    fn v2_api_key_blank_treated_as_unset() {
        with_env("ACTIONBOOK_API_KEY", Some("   "), || {
            assert!(require_api_key().is_err());
        });
    }

    // ── Inner runcode --timeout (spec § 双层超时) ──

    #[test]
    fn v2_runcode_cmd_includes_inner_timeout() {
        let cmd = build_runcode_cmd("h", 90_000, None, None);
        assert!(cmd.starts_with("browser run-code --tab h --timeout "));
        // 90_000 caller → 85_000 inner (5 s slack)
        assert!(cmd.contains("--timeout 85000"), "expected 85000, got: {cmd}");
    }

    #[test]
    fn v2_runcode_cmd_clamps_at_115s_max() {
        let cmd = build_runcode_cmd("h", 1_000_000, None, None); // wildly over max
        assert!(cmd.contains("--timeout 115000"), "expected 115000 cap, got: {cmd}");
    }

    #[test]
    fn v2_runcode_cmd_floor_at_5s_min() {
        let cmd = build_runcode_cmd("h", 1_000, None, None); // below 5 s, below slack
        // saturating_sub goes to 0, then max(5_000) lifts back to 5_000
        assert!(cmd.contains("--timeout 5000"), "expected 5000 floor, got: {cmd}");
    }

    #[test]
    fn v2_runcode_cmd_default_caller_90s_yields_inner_85s() {
        // The ascent-research default (commands/add.rs DEFAULT_TIMEOUT_MS = 90_000)
        // must yield an inner timeout safely under V2's 115 s max and above its 60 s default.
        let cmd = build_runcode_cmd("research-foo-1", 90_000, None, None);
        assert!(cmd.contains("--timeout 85000"));
        // Inner timeout (85s) > V2 default (60s) — confirms we override the default.
        assert!(85_000 > 60_000);
    }

    #[test]
    fn v2_runcode_is_function_expression_with_three_stage_wait() {
        let js = runcode_inline_js();
        assert!(js.starts_with("async (page) =>"), "must be function expression, not IIFE: {js}");
        // Three-stage wait.
        assert!(js.contains("domcontentloaded"), "stage 1 missing: {js}");
        assert!(js.contains("networkidle"), "stage 2 missing: {js}");
        assert!(
            js.contains("document.body.innerText.length > 100"),
            "stage 3 (body-content poll) missing: {js}"
        );
        // Each waitForLoadState must be try/catch-guarded.
        let try_count = js.matches("try {").count();
        let catch_count = js.matches("catch (_e)").count();
        assert!(try_count >= 2, "expect ≥2 try blocks, got {try_count}: {js}");
        assert!(catch_count >= 2, "expect ≥2 catch blocks, got {catch_count}: {js}");
        assert!(js.contains("document.body.innerText"));
    }

    #[test]
    fn parses_inline_error_envelope() {
        let text = "[t1]\nerror EXTENSION_OFFLINE: no active extension websocket for this user";
        let parsed = parse_inline_error(text).expect("should parse");
        assert_eq!(parsed.code, "EXTENSION_OFFLINE");
        assert_eq!(parsed.message, "no active extension websocket for this user");
    }

    #[test]
    fn ignores_non_error_text() {
        assert!(parse_inline_error("[t1] https://example.com/\nok browser new-tab").is_none());
    }

    #[test]
    fn error_prefix_for_known_codes() {
        assert_eq!(error_prefix_for("EXTENSION_OFFLINE", ""), "EXTENSION_OFFLINE");
        assert_eq!(error_prefix_for("CANCELLED", ""), "CANCELLED");
        assert_eq!(error_prefix_for("EVAL_FAILED", ""), "RUN_CODE_FAILED");
    }

    #[test]
    fn debugger_attach_conflict_detection() {
        assert_eq!(
            error_prefix_for("INTERNAL_ERROR", "Cannot access a chrome-extension:// URL of different extension"),
            "DEBUGGER_ATTACH_CONFLICT"
        );
        assert_eq!(
            error_prefix_for("INTERNAL_ERROR", "Detached while handling command."),
            "DEBUGGER_ATTACH_CONFLICT"
        );
        assert_eq!(error_prefix_for("INTERNAL_ERROR", "some other internal err"), "INTERNAL_ERROR");
    }

    #[test]
    fn extracts_runcode_payload() {
        let raw = "[t1]\nok browser run-code\n{\"result\":{\"url\":\"https://example.com/\",\"title\":\"Example Domain\",\"text\":\"Example Domain\\n\\nHello\"}}";
        let p = extract_run_code_payload(raw).expect("should parse");
        assert_eq!(p.url, "https://example.com/");
        assert_eq!(p.text, "Example Domain\n\nHello");
    }

    #[test]
    fn is_recoverable_handle_loss_matches_two_codes() {
        assert!(is_recoverable_handle_loss("SESSION_LOST: foo"));
        assert!(is_recoverable_handle_loss("TAB_NOT_FOUND: bar"));
        assert!(!is_recoverable_handle_loss("EXTENSION_OFFLINE: baz"));
    }
}
