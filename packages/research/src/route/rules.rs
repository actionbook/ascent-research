//! TOML preset schema + matcher + classify().
//!
//! ## Preset file schema
//!
//! ```toml
//! name = "tech"
//! description = "..."
//!
//! [[rule]]
//! kind = "hn-item"
//! host = "news.ycombinator.com"
//! path = "/item"                          # OR path_any_of = [...] OR path_segments = [...]
//! query_param = { id = "[0-9]+" }         # optional; each value is a Rust regex,
//!                                          # implicitly anchored to full value
//! executor = "postagent"                  # or "browser"
//! template = 'postagent send "..."'
//!
//! [fallback]
//! executor = "browser"
//! kind = "browser-fallback"
//! template = "..."
//! ```
//!
//! Placeholders in `template` may be drawn from:
//! - `{url}`, `{host}`, `{path}` (universal)
//! - path_segments captures like `{owner}` `{repo}` `{num}` `{id}`
//! - query_param keys
//!
//! Any unbound placeholder in template = PLACEHOLDER_UNBOUND at load time.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

// ── Schema ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "rule")]
    pub rules: Vec<RuleSpec>,
    pub fallback: FallbackSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSpec {
    pub kind: String,
    pub host: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub path_any_of: Option<Vec<String>>,
    #[serde(default)]
    pub path_segments: Option<Vec<String>>,
    #[serde(default)]
    pub query_param: Option<HashMap<String, String>>,
    pub executor: String,
    pub template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackSpec {
    pub kind: String,
    pub executor: String,
    pub template: String,
}

// ── Compiled preset (post-validation) ───────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CompiledPreset {
    pub name: String,
    pub rules: Vec<CompiledRule>,
    pub fallback: FallbackSpec,
}

#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub kind: String,
    pub host: String,
    pub path_matcher: PathMatcher,
    pub query_regexes: Vec<(String, Regex)>,
    pub executor: String,
    pub template: String,
}

#[derive(Debug, Clone)]
pub enum PathMatcher {
    Exact(String),
    AnyOf(Vec<String>),
    Segments(Vec<SegmentPattern>),
}

#[derive(Debug, Clone)]
pub enum SegmentPattern {
    Literal(String),
    /// Placeholder like `{owner}` — captures one segment by this name.
    Capture(String),
    /// Variable-length placeholder like `{...path}` — captures all remaining
    /// segments joined by `/`. Only valid as the last segment in a pattern.
    VarCapture(String),
}

// ── Classify result ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Executor {
    Postagent,
    Browser,
    /// v3: in-process local file / directory read. No subprocess spawn;
    /// the `fetch::local` module reads the path directly. Gated behind
    /// `file://` URL or absolute/relative path classification.
    Local,
}

impl Executor {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "postagent" => Some(Executor::Postagent),
            "browser" => Some(Executor::Browser),
            "local" => Some(Executor::Local),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Executor::Postagent => "postagent",
            Executor::Browser => "browser",
            Executor::Local => "local",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Route {
    pub executor: Executor,
    pub kind: String,
    pub command_template: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub enum Classification {
    Matched(Route),
    Fallback(Route),
    Forced(Route),
}

impl Classification {
    pub fn route(&self) -> &Route {
        match self {
            Classification::Matched(r)
            | Classification::Fallback(r)
            | Classification::Forced(r) => r,
        }
    }
}

// ── Preset errors ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PresetSubCode {
    FileNotFound,
    TomlSyntax,
    SchemaInvalid,
    PlaceholderUnbound,
}

impl PresetSubCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PresetSubCode::FileNotFound => "FILE_NOT_FOUND",
            PresetSubCode::TomlSyntax => "TOML_SYNTAX",
            PresetSubCode::SchemaInvalid => "SCHEMA_INVALID",
            PresetSubCode::PlaceholderUnbound => "PLACEHOLDER_UNBOUND",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PresetError {
    pub sub_code: PresetSubCode,
    pub message: String,
    pub path: Option<String>,
}

impl std::fmt::Display for PresetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.path {
            Some(p) => write!(f, "[{}] {} ({})", self.sub_code.as_str(), self.message, p),
            None => write!(f, "[{}] {}", self.sub_code.as_str(), self.message),
        }
    }
}
impl std::error::Error for PresetError {}

// ── Loading ─────────────────────────────────────────────────────────────────

const BUILTIN_TECH: &str = include_str!("../../presets/tech.toml");
const BUILTIN_SPORTS: &str = include_str!("../../presets/sports.toml");

/// Load and compile a preset, honoring resolution order:
/// 1. `rules_path` (explicit file) if provided
/// 2. Otherwise, `preset` name:
///    - `<research_root>/presets/<name>.toml` (user override under the
///      canonical `~/.actionbook/ascent-research/` home)
///    - `~/.actionbook/research/presets/<name>.toml` (legacy v0.2 path,
///      read-only with a one-shot warning so upgraded users don't lose
///      their existing overrides)
///    - built-in presets shipped embedded
pub fn load_preset(
    preset: Option<&str>,
    rules_path: Option<&Path>,
) -> Result<CompiledPreset, PresetError> {
    if let Some(path) = rules_path {
        let text = std::fs::read_to_string(path).map_err(|e| PresetError {
            sub_code: PresetSubCode::FileNotFound,
            message: format!("cannot open rules file: {e}"),
            path: Some(path.display().to_string()),
        })?;
        return parse_and_compile(&text, Some(path.display().to_string()));
    }

    let name = preset.unwrap_or("tech");

    // Canonical v0.3+ location: <research_root>/presets/<name>.toml. This
    // follows `ACTIONBOOK_RESEARCH_HOME` if set, so tests and sandboxes
    // stay isolated.
    let user_path = crate::session::layout::research_root()
        .join("presets")
        .join(format!("{name}.toml"));
    if user_path.exists() {
        let text = std::fs::read_to_string(&user_path).map_err(|e| PresetError {
            sub_code: PresetSubCode::FileNotFound,
            message: format!("cannot read user preset: {e}"),
            path: Some(user_path.display().to_string()),
        })?;
        return parse_and_compile(&text, Some(user_path.display().to_string()));
    }

    // Legacy v0.2 fallback: `~/.actionbook/research/presets/<name>.toml`.
    // Read-only — nothing ever writes here after v0.3. Emit a one-shot
    // stderr notice so the user knows to move it. Honors the same
    // `ACTIONBOOK_RESEARCH_HOME` escape the session layout uses: when
    // that override is set, we skip the legacy path entirely.
    if std::env::var("ACTIONBOOK_RESEARCH_HOME").is_err()
        && let Some(home) = dirs::home_dir()
    {
        let legacy_path = home
            .join(".actionbook/research/presets")
            .join(format!("{name}.toml"));
        if legacy_path.exists() {
            eprintln!(
                "warning: using legacy preset at {} — move to {} to silence this notice (legacy path will be removed in v0.4)",
                legacy_path.display(),
                user_path.display()
            );
            let text = std::fs::read_to_string(&legacy_path).map_err(|e| PresetError {
                sub_code: PresetSubCode::FileNotFound,
                message: format!("cannot read user preset: {e}"),
                path: Some(legacy_path.display().to_string()),
            })?;
            return parse_and_compile(&text, Some(legacy_path.display().to_string()));
        }
    }

    // built-in
    match name {
        "tech" => parse_and_compile(BUILTIN_TECH, Some("<builtin:tech>".to_string())),
        "sports" => parse_and_compile(BUILTIN_SPORTS, Some("<builtin:sports>".to_string())),
        other => Err(PresetError {
            sub_code: PresetSubCode::FileNotFound,
            message: format!("no preset named '{other}' (ship your own TOML with --rules)"),
            path: None,
        }),
    }
}

fn parse_and_compile(text: &str, src: Option<String>) -> Result<CompiledPreset, PresetError> {
    let p: Preset = toml::from_str(text).map_err(|e| PresetError {
        sub_code: PresetSubCode::TomlSyntax,
        message: format!("{e}"),
        path: src.clone(),
    })?;
    compile(p, src)
}

fn compile(p: Preset, src: Option<String>) -> Result<CompiledPreset, PresetError> {
    let mut compiled_rules = Vec::with_capacity(p.rules.len());
    for (idx, r) in p.rules.iter().enumerate() {
        let compiled = compile_rule(r, idx, src.as_deref())?;
        compiled_rules.push(compiled);
    }
    Ok(CompiledPreset {
        name: p.name,
        rules: compiled_rules,
        fallback: p.fallback,
    })
}

fn compile_rule(r: &RuleSpec, idx: usize, src: Option<&str>) -> Result<CompiledRule, PresetError> {
    // Validate path-matcher kind
    let matcher_specified = [
        r.path.is_some(),
        r.path_any_of.is_some(),
        r.path_segments.is_some(),
    ]
    .iter()
    .filter(|x| **x)
    .count();
    if matcher_specified != 1 {
        return Err(PresetError {
            sub_code: PresetSubCode::SchemaInvalid,
            message: format!(
                "rule[{idx}] (kind={}) must specify exactly one of path / path_any_of / path_segments",
                r.kind
            ),
            path: src.map(String::from),
        });
    }
    let path_matcher = if let Some(p) = &r.path {
        PathMatcher::Exact(p.clone())
    } else if let Some(any) = &r.path_any_of {
        PathMatcher::AnyOf(any.clone())
    } else {
        let segs: Vec<SegmentPattern> = r
            .path_segments
            .as_ref()
            .unwrap()
            .iter()
            .map(|s| {
                if s.starts_with('{') && s.ends_with('}') {
                    let inner = &s[1..s.len() - 1];
                    if let Some(name) = inner.strip_prefix("...") {
                        SegmentPattern::VarCapture(name.to_string())
                    } else {
                        SegmentPattern::Capture(inner.to_string())
                    }
                } else {
                    SegmentPattern::Literal(s.clone())
                }
            })
            .collect();

        // VarCapture must only appear as the last segment.
        if let Some(bad_idx) = segs
            .iter()
            .enumerate()
            .position(|(i, p)| matches!(p, SegmentPattern::VarCapture(_)) && i != segs.len() - 1)
        {
            return Err(PresetError {
                sub_code: PresetSubCode::SchemaInvalid,
                message: format!(
                    "rule[{idx}] (kind={}) variable-length {{...name}} segment at position {bad_idx} \
                    must be the last path segment",
                    r.kind
                ),
                path: src.map(String::from),
            });
        }

        PathMatcher::Segments(segs)
    };

    // Compile query regexes (implicit ^...$)
    let mut query_regexes = Vec::new();
    if let Some(qs) = &r.query_param {
        for (k, pat) in qs {
            let anchored = format!("^(?:{pat})$");
            let re = Regex::new(&anchored).map_err(|e| PresetError {
                sub_code: PresetSubCode::SchemaInvalid,
                message: format!(
                    "rule[{idx}] (kind={}) query_param.{k}: invalid regex: {e}",
                    r.kind
                ),
                path: src.map(String::from),
            })?;
            query_regexes.push((k.clone(), re));
        }
    }

    // Placeholder binding check
    let bound = bound_placeholders(&path_matcher, &query_regexes);
    let used = extract_placeholders(&r.template);
    for placeholder in &used {
        if !bound.contains(placeholder) && !is_universal(placeholder) {
            return Err(PresetError {
                sub_code: PresetSubCode::PlaceholderUnbound,
                message: format!(
                    "rule[{idx}] (kind={}) template has `{{{placeholder}}}` but it isn't in \
                    path_segments, query_param, or universal {{url,host,path}}",
                    r.kind
                ),
                path: src.map(String::from),
            });
        }
    }

    if Executor::parse(&r.executor).is_none() {
        return Err(PresetError {
            sub_code: PresetSubCode::SchemaInvalid,
            message: format!(
                "rule[{idx}] (kind={}) executor must be 'postagent' or 'browser', got '{}'",
                r.kind, r.executor
            ),
            path: src.map(String::from),
        });
    }

    Ok(CompiledRule {
        kind: r.kind.clone(),
        host: r.host.to_lowercase(),
        path_matcher,
        query_regexes,
        executor: r.executor.clone(),
        template: r.template.clone(),
    })
}

fn bound_placeholders(
    path: &PathMatcher,
    queries: &[(String, Regex)],
) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    if let PathMatcher::Segments(segs) = path {
        for s in segs {
            match s {
                SegmentPattern::Capture(name) | SegmentPattern::VarCapture(name) => {
                    set.insert(name.clone());
                }
                SegmentPattern::Literal(_) => {}
            }
        }
    }
    for (k, _) in queries {
        set.insert(k.clone());
    }
    set
}

fn extract_placeholders(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{'
            && let Some(end) = bytes[i + 1..].iter().position(|&b| b == b'}')
        {
            let name = &template[i + 1..i + 1 + end];
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                out.push(name.to_string());
            }
            i += end + 2;
            continue;
        }
        i += 1;
    }
    out
}

fn is_universal(name: &str) -> bool {
    matches!(name, "url" | "host" | "path")
}

// ── URL parsing (minimal; http/https only) ──────────────────────────────────

#[derive(Debug, Clone)]
pub struct ParsedUrl {
    pub host: String,
    pub path: String,
    pub query: String,
}

impl ParsedUrl {
    pub fn parse(url: &str) -> Option<Self> {
        let url = url.trim();
        let rest = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))?;
        let (authority_and_path, query) = match rest.split_once('?') {
            Some((prefix, q)) => (prefix, q.to_string()),
            None => (rest, String::new()),
        };
        let authority_and_path = authority_and_path.split('#').next().unwrap_or("");
        let (authority, path_raw) = match authority_and_path.find('/') {
            Some(i) => (&authority_and_path[..i], &authority_and_path[i..]),
            None => (authority_and_path, ""),
        };
        if authority.is_empty() {
            return None;
        }
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = host.split(':').next().unwrap_or(host);
        Some(ParsedUrl {
            host: host.to_ascii_lowercase(),
            path: path_raw.to_string(),
            query,
        })
    }

    pub fn first_query_value(&self, key: &str) -> Option<&str> {
        for pair in self.query.split('&') {
            if let Some((k, v)) = pair.split_once('=')
                && k == key
            {
                return Some(v);
            }
        }
        None
    }
}

// ── Classification ──────────────────────────────────────────────────────────

/// Classify a URL against a compiled preset.
pub fn classify(
    preset: &CompiledPreset,
    url: &str,
    prefer_browser: bool,
) -> Result<Classification, String> {
    // v3: local paths / file:// URLs short-circuit the HTTP classification.
    // These are handled in-process by `fetch::local`, never routed through
    // postagent or a browser subprocess.
    if let Some(route) = classify_as_local(url) {
        return Ok(Classification::Matched(route));
    }

    let parsed =
        ParsedUrl::parse(url).ok_or_else(|| format!("cannot parse '{url}' as http(s) URL"))?;

    if prefer_browser {
        let route = Route {
            executor: Executor::Browser,
            kind: "browser-forced".into(),
            command_template: interpolate(
                &preset.fallback.template,
                &url_to_map(url, &parsed, &HashMap::new()),
            ),
            url: url.into(),
        };
        return Ok(Classification::Forced(route));
    }

    for rule in &preset.rules {
        if let Some(captures) = match_rule(rule, &parsed) {
            let tpl_map = url_to_map(url, &parsed, &captures);
            let route = Route {
                executor: Executor::parse(&rule.executor).expect("validated at load"),
                kind: rule.kind.clone(),
                command_template: interpolate(&rule.template, &tpl_map),
                url: url.into(),
            };
            return Ok(Classification::Matched(route));
        }
    }

    let route = Route {
        executor: Executor::parse(&preset.fallback.executor)
            .ok_or_else(|| "fallback executor must be postagent or browser".to_string())?,
        kind: preset.fallback.kind.clone(),
        command_template: interpolate(
            &preset.fallback.template,
            &url_to_map(url, &parsed, &HashMap::new()),
        ),
        url: url.into(),
    };
    Ok(Classification::Fallback(route))
}

/// Try to classify `input` as a local file/dir path or `file://` URL.
/// Returns None if the input isn't locally addressable (let HTTP path
/// handle it).
///
/// Accepts:
/// - `file:///abs/path` or `file://host/abs/path` (host ignored)
/// - absolute unix path `/abs/path`
/// - relative path `./x`, `../x`
/// - home-relative `~/x` (expanded)
///
/// `kind`: `local-file` (file) or `local-tree` (directory). If the path
/// doesn't exist on disk we still return Some with `local-file` so the
/// caller's error path produces a clean SourceRejected rather than
/// "cannot parse as http URL".
fn classify_as_local(input: &str) -> Option<Route> {
    let abs = normalize_local_path(input)?;
    let kind = match std::fs::metadata(&abs) {
        Ok(m) if m.is_dir() => "local-tree",
        // File, or missing (let the fetch layer emit fetch_failed)
        _ => "local-file",
    };
    // Re-encode as file:// URL so downstream events carry a consistent
    // addressing scheme; observed_url in jsonl will match this.
    let canonical = format!("file://{}", abs.to_string_lossy());
    Some(Route {
        executor: Executor::Local,
        kind: kind.to_string(),
        command_template: format!("<local read {}>", abs.display()),
        url: canonical,
    })
}

fn normalize_local_path(input: &str) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    // file:// URL
    let raw = if let Some(rest) = input.strip_prefix("file://") {
        // Strip optional host segment: file://host/path → /path.
        // Empty host (file:///abs) leaves leading / intact.
        match rest.find('/') {
            Some(0) => rest.to_string(),
            Some(i) => rest[i..].to_string(),
            None => return None,
        }
    } else if input.starts_with('/') || input.starts_with("./") || input.starts_with("../") {
        input.to_string()
    } else if let Some(rest) = input.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()?;
        format!("{home}/{rest}")
    } else {
        return None;
    };
    let path = PathBuf::from(raw);
    // Only accept clearly-local paths. Leave URL-encoded weirdness alone.
    if path.as_os_str().is_empty() {
        return None;
    }
    Some(path)
}

fn match_rule(rule: &CompiledRule, parsed: &ParsedUrl) -> Option<HashMap<String, String>> {
    if parsed.host != rule.host {
        return None;
    }
    // path
    let mut caps = HashMap::new();
    match &rule.path_matcher {
        PathMatcher::Exact(p) => {
            if &parsed.path != p {
                return None;
            }
        }
        PathMatcher::AnyOf(list) => {
            if !list.contains(&parsed.path) {
                return None;
            }
        }
        PathMatcher::Segments(patterns) => {
            let segs: Vec<&str> = parsed
                .path
                .trim_matches('/')
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();

            let has_var_tail = matches!(patterns.last(), Some(SegmentPattern::VarCapture(_)));

            if has_var_tail {
                // Need at least (patterns.len() - 1) fixed segments.
                if segs.len() < patterns.len() - 1 {
                    return None;
                }
                let fixed_count = patterns.len() - 1;
                for (pat, seg) in patterns[..fixed_count]
                    .iter()
                    .zip(segs[..fixed_count].iter())
                {
                    match pat {
                        SegmentPattern::Literal(lit) => {
                            if lit != seg {
                                return None;
                            }
                        }
                        SegmentPattern::Capture(name) => {
                            caps.insert(name.clone(), (*seg).to_string());
                        }
                        SegmentPattern::VarCapture(_) => unreachable!(),
                    }
                }
                if let Some(SegmentPattern::VarCapture(name)) = patterns.last() {
                    caps.insert(name.clone(), segs[fixed_count..].join("/"));
                }
            } else {
                if segs.len() != patterns.len() {
                    return None;
                }
                for (pat, seg) in patterns.iter().zip(segs.iter()) {
                    match pat {
                        SegmentPattern::Literal(lit) => {
                            if lit != seg {
                                return None;
                            }
                        }
                        SegmentPattern::Capture(name) => {
                            caps.insert(name.clone(), (*seg).to_string());
                        }
                        SegmentPattern::VarCapture(_) => unreachable!(),
                    }
                }
            }
        }
    }

    // query_param
    for (key, re) in &rule.query_regexes {
        let val = parsed.first_query_value(key)?;
        if !re.is_match(val) {
            return None;
        }
        caps.insert(key.clone(), val.to_string());
    }

    Some(caps)
}

fn url_to_map(
    url: &str,
    parsed: &ParsedUrl,
    captures: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("url".into(), url.into());
    m.insert("host".into(), parsed.host.clone());
    m.insert("path".into(), parsed.path.clone());
    for (k, v) in captures {
        m.insert(k.clone(), v.clone());
    }
    m
}

fn interpolate(template: &str, vars: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{'
            && let Some(end) = bytes[i + 1..].iter().position(|&b| b == b'}')
        {
            let name = &template[i + 1..i + 1 + end];
            if let Some(val) = vars.get(name) {
                out.push_str(val);
                i += end + 2;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tech() -> CompiledPreset {
        load_preset(Some("tech"), None).expect("builtin tech must load")
    }

    fn sports() -> CompiledPreset {
        load_preset(Some("sports"), None).expect("builtin sports must load")
    }

    #[test]
    fn builtin_tech_loads() {
        let p = tech();
        assert_eq!(p.name, "tech");
        assert!(!p.rules.is_empty());
    }

    #[test]
    fn builtin_sports_loads() {
        let p = sports();
        assert_eq!(p.name, "sports");
        assert!(!p.rules.is_empty());
    }

    #[test]
    fn hn_item_route() {
        let c = classify(&tech(), "https://news.ycombinator.com/item?id=12345", false).unwrap();
        let r = c.route();
        assert_eq!(r.executor, Executor::Browser);
        assert_eq!(r.kind, "hn-item");
        assert!(
            r.command_template
                .contains("news.ycombinator.com/item?id=12345")
        );
    }

    #[test]
    fn hn_item_non_numeric_id_falls_back() {
        let c = classify(&tech(), "https://news.ycombinator.com/item?id=abc", false).unwrap();
        assert_eq!(c.route().executor, Executor::Browser);
    }

    #[test]
    fn hn_topstories_routes() {
        for url in [
            "https://news.ycombinator.com/",
            "https://news.ycombinator.com/news",
        ] {
            let c = classify(&tech(), url, false).unwrap();
            assert_eq!(c.route().kind, "hn-topstories", "for {url}");
        }
    }

    #[test]
    fn github_repo_readme() {
        let c = classify(&tech(), "https://github.com/bytedance/monoio", false).unwrap();
        assert_eq!(c.route().kind, "github-repo-readme");
        assert!(
            c.route()
                .command_template
                .contains("/repos/bytedance/monoio/readme")
        );
    }

    #[test]
    fn github_issue() {
        let c = classify(
            &tech(),
            "https://github.com/tokio-rs/tokio/issues/8056",
            false,
        )
        .unwrap();
        assert_eq!(c.route().kind, "github-issue");
        assert!(
            c.route()
                .command_template
                .contains("/repos/tokio-rs/tokio/issues/8056")
        );
        assert!(
            c.route()
                .command_template
                .contains("$POSTAGENT.GITHUB.TOKEN")
        );
    }

    #[test]
    fn arxiv_abs() {
        let c = classify(&tech(), "https://arxiv.org/abs/2601.12345", false).unwrap();
        assert_eq!(c.route().kind, "arxiv-abs");
        assert_eq!(c.route().executor, Executor::Browser);
        assert!(
            c.route()
                .command_template
                .contains("arxiv.org/abs/2601.12345")
        );
    }

    #[test]
    fn unknown_falls_back() {
        let c = classify(&tech(), "https://corrode.dev/blog/async/", false).unwrap();
        assert!(matches!(c, Classification::Fallback(_)));
        assert_eq!(c.route().executor, Executor::Browser);
        assert_eq!(c.route().kind, "browser-fallback");
    }

    #[test]
    fn prefer_browser_forces() {
        let c = classify(&tech(), "https://github.com/foo/bar", true).unwrap();
        assert!(matches!(c, Classification::Forced(_)));
        assert_eq!(c.route().kind, "browser-forced");
    }

    #[test]
    fn invalid_url_errors() {
        let err = classify(&tech(), "not-a-url", false).unwrap_err();
        assert!(err.contains("cannot parse"));
    }

    #[test]
    fn placeholder_unbound_fails_load() {
        let bad = r#"
name = "bad"
[[rule]]
kind = "x"
host = "example.com"
path = "/x"
executor = "postagent"
template = "echo {missing}"
[fallback]
kind = "fb"
executor = "browser"
template = "fb"
"#;
        let err = parse_and_compile(bad, Some("test".into())).unwrap_err();
        assert_eq!(err.sub_code, PresetSubCode::PlaceholderUnbound);
    }

    #[test]
    fn toml_syntax_error() {
        let err = parse_and_compile("this is not = valid = toml\n[[", None).unwrap_err();
        assert_eq!(err.sub_code, PresetSubCode::TomlSyntax);
    }

    #[test]
    fn schema_invalid_missing_path_matcher() {
        let bad = r#"
name = "bad"
[[rule]]
kind = "x"
host = "example.com"
executor = "postagent"
template = "echo"
[fallback]
kind = "fb"
executor = "browser"
template = "fb"
"#;
        let err = parse_and_compile(bad, None).unwrap_err();
        assert_eq!(err.sub_code, PresetSubCode::SchemaInvalid);
    }

    #[test]
    fn file_not_found() {
        let err = load_preset(None, Some(Path::new("/no/such/path.toml"))).unwrap_err();
        assert_eq!(err.sub_code, PresetSubCode::FileNotFound);
    }

    #[test]
    fn github_file_blob_routes_to_raw() {
        let c = classify(
            &tech(),
            "https://github.com/tokio-rs/tokio/blob/master/tokio/src/runtime/mod.rs",
            false,
        )
        .unwrap();
        assert_eq!(c.route().kind, "github-file");
        assert_eq!(c.route().executor, Executor::Browser);
        assert!(
            c.route().command_template.contains(
                "raw.githubusercontent.com/tokio-rs/tokio/master/tokio/src/runtime/mod.rs"
            ),
            "got: {}",
            c.route().command_template
        );
    }

    #[test]
    fn github_tree_routes_to_contents_api() {
        let c = classify(
            &tech(),
            "https://github.com/tokio-rs/tokio/tree/master/tokio/src/runtime",
            false,
        )
        .unwrap();
        assert_eq!(c.route().kind, "github-tree");
        assert!(
            c.route().command_template.contains(
                "api.github.com/repos/tokio-rs/tokio/contents/tokio/src/runtime?ref=master"
            )
        );
        assert!(
            c.route()
                .command_template
                .contains("$POSTAGENT.GITHUB.TOKEN")
        );
    }

    #[test]
    fn github_tree_empty_tail_routes() {
        let c = classify(
            &tech(),
            "https://github.com/tokio-rs/tokio/tree/master",
            false,
        )
        .unwrap();
        assert_eq!(c.route().kind, "github-tree");
        assert!(
            c.route()
                .command_template
                .contains("api.github.com/repos/tokio-rs/tokio/contents/?ref=master")
        );
    }

    #[test]
    fn github_raw_passthrough() {
        let c = classify(
            &tech(),
            "https://raw.githubusercontent.com/rust-lang/rust/master/README.md",
            false,
        )
        .unwrap();
        assert_eq!(c.route().kind, "github-raw");
        assert_eq!(c.route().executor, Executor::Browser);
        assert!(
            c.route()
                .command_template
                .contains("raw.githubusercontent.com/rust-lang/rust/master/README.md")
        );
    }

    #[test]
    fn github_repo_readme_still_wins_for_two_segments() {
        // Adding the new 5-segment rules must not shadow the 2-segment repo-readme.
        let c = classify(&tech(), "https://github.com/bytedance/monoio", false).unwrap();
        assert_eq!(c.route().kind, "github-repo-readme");
    }

    #[test]
    fn var_capture_non_tail_fails_load() {
        let bad = r#"
name = "bad"
[[rule]]
kind = "x"
host = "example.com"
path_segments = ["{...head}", "tail"]
executor = "postagent"
template = 'echo {head}'
[fallback]
kind = "fb"
executor = "browser"
template = "fb"
"#;
        let err = parse_and_compile(bad, Some("test".into())).unwrap_err();
        assert_eq!(err.sub_code, PresetSubCode::SchemaInvalid);
        assert!(err.message.contains("must be the last path segment"));
    }

    #[test]
    fn universal_placeholders_always_bound() {
        let p = r#"
name = "uni"
[[rule]]
kind = "k"
host = "example.com"
path = "/p"
executor = "browser"
template = 'fetch "{url}" host={host} path={path}'
[fallback]
kind = "fb"
executor = "browser"
template = "fb"
"#;
        let preset = parse_and_compile(p, None).unwrap();
        let c = classify(&preset, "https://example.com/p", false).unwrap();
        let tpl = &c.route().command_template;
        assert!(tpl.contains("fetch \"https://example.com/p\""));
        assert!(tpl.contains("host=example.com"));
        assert!(tpl.contains("path=/p"));
    }

    // v3: local classification tests

    fn test_preset_for_local() -> CompiledPreset {
        // Any valid preset works — local classification short-circuits
        // before the preset is consulted. Use the minimum schema.
        let p = r#"
name = "test-local"

[[rules]]
kind = "x"
host = "example.com"
path = { exact = "/x" }
executor = "postagent"
template = "x"

[fallback]
kind = "fb"
executor = "browser"
template = "fb"
"#;
        parse_and_compile(p, None).unwrap()
    }

    #[test]
    fn classifies_file_scheme_as_local_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("file://{}", tmp.path().display());
        let c = classify(&test_preset_for_local(), &url, false).unwrap();
        let route = c.route();
        assert_eq!(route.executor, Executor::Local);
        assert_eq!(route.kind, "local-file");
        assert!(route.url.starts_with("file://"));
    }

    #[test]
    fn classifies_file_scheme_pointing_at_dir_as_local_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let url = format!("file://{}", tmp.path().display());
        let c = classify(&test_preset_for_local(), &url, false).unwrap();
        assert_eq!(c.route().kind, "local-tree");
    }

    #[test]
    fn classifies_absolute_unix_path_as_local() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let c = classify(
            &test_preset_for_local(),
            tmp.path().to_str().unwrap(),
            false,
        )
        .unwrap();
        assert_eq!(c.route().executor, Executor::Local);
    }

    #[test]
    fn classifies_missing_path_as_local_file_anyway() {
        // A path we'd expect never to exist should still route as local
        // so the fetch layer can return a clean fetch_failed.
        let c = classify(
            &test_preset_for_local(),
            "/nonexistent/path/xyz-123-research-test",
            false,
        )
        .unwrap();
        assert_eq!(c.route().executor, Executor::Local);
        assert_eq!(c.route().kind, "local-file");
    }

    #[test]
    fn does_not_misclassify_https_as_local() {
        let c = classify(&test_preset_for_local(), "https://example.com/x", false).unwrap();
        // Should fall through to the rule / fallback, not Local.
        assert_ne!(c.route().executor, Executor::Local);
    }
}
