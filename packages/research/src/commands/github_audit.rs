#![allow(clippy::result_large_err)]

use chrono::{DateTime, Utc};
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;

use crate::fetch::postagent;
use crate::output::Envelope;

const CMD: &str = "research github-audit";
const DEPTHS: &[&str] = &["repo", "stargazers", "timeline"];
const GITHUB_API: &str = "https://api.github.com";
const FETCH_TIMEOUT_MS: u64 = 30_000;
const GITHUB_JSON_ACCEPT: &str = "application/vnd.github+json";
const GITHUB_STAR_ACCEPT: &str = "application/vnd.github.star+json";
const POSTAGENT_GITHUB_AUTH_HEADER: &str = "Authorization: Bearer $POSTAGENT.GITHUB.TOKEN";

#[derive(Debug, Clone)]
struct RepoInput {
    owner: String,
    repo: String,
}

struct GithubResponse {
    endpoint: EndpointRecord,
    value: Option<Value>,
}

enum GithubFetchError {
    CredentialRequired,
    Other(String),
}

struct StargazerSample {
    login: String,
    starred_at: Option<String>,
}

struct StargazerProfile {
    created_at: Option<DateTime<Utc>>,
    followers: u64,
    public_repos: u64,
    empty_bio: bool,
}

struct StargazerCollection {
    samples: Vec<StargazerSample>,
    profiles: Vec<StargazerProfile>,
    pages: usize,
    endpoints: Vec<Value>,
}

#[derive(Clone)]
struct EndpointRecord {
    path: String,
    status: Option<i32>,
    body_bytes: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StatsAvailability {
    Available,
    Pending,
    Unavailable,
}

pub fn run(
    repo: &str,
    depth: &str,
    sample: usize,
    out: Option<&str>,
    html: Option<&str>,
) -> Envelope {
    if !DEPTHS.contains(&depth) {
        return Envelope::fail(
            CMD,
            "INVALID_ARGUMENT",
            "invalid --depth; expected one of: repo, stargazers, timeline",
        )
        .with_details(json!({
            "argument": "depth",
            "value": depth,
            "allowed": DEPTHS,
        }));
    }

    if !(1..=1000).contains(&sample) {
        return Envelope::fail(
            CMD,
            "INVALID_ARGUMENT",
            "--sample must be between 1 and 1000",
        )
        .with_details(json!({
            "argument": "sample",
            "value": sample,
            "min": 1,
            "max": 1000,
        }));
    }

    let repo_input = match parse_repo_input(repo) {
        Ok(repo_input) => repo_input,
        Err(envelope) => return envelope,
    };

    let envelope = if depth == "timeline" {
        collect_timeline_depth(&repo_input, sample)
    } else if depth == "stargazers" {
        collect_stargazers_depth(&repo_input, sample)
    } else {
        collect_repo_depth(&repo_input)
    };

    write_outputs_if_requested(envelope, out, html)
}

pub fn render_plain_summary(envelope: &Envelope) {
    if !envelope.ok {
        envelope.render(false);
        return;
    }

    let data = &envelope.data;
    let owner = data
        .get("repository")
        .and_then(|repo| repo.get("owner"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let repo = data
        .get("repository")
        .and_then(|repo| repo.get("repo"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let depth = data
        .get("depth")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let risk = data.get("risk").unwrap_or(&Value::Null);
    let score = risk.get("score").and_then(Value::as_i64).unwrap_or(0);
    let band = risk
        .get("band")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let confidence = risk
        .get("confidence")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let trust = data.get("trust").unwrap_or(&Value::Null);
    let trust_score = trust
        .get("score")
        .and_then(Value::as_i64)
        .unwrap_or(100 - score);
    let trust_band = trust
        .get("band")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let evidence_status = risk
        .get("evidence_status")
        .and_then(Value::as_str)
        .unwrap_or("complete");
    let reasons = risk
        .get("reasons")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "none".to_string());

    println!("repo: {owner}/{repo}");
    println!("depth: {depth}");
    println!("trust: score={trust_score} band={trust_band}");
    println!(
        "risk: score={score} band={band} confidence={confidence:.2} evidence_status={evidence_status}"
    );
    println!("reasons: {reasons}");
    if let Some(path) = data.get("out").and_then(Value::as_str) {
        println!("out: {path}");
    }
    if let Some(path) = data.get("html_out").and_then(Value::as_str) {
        println!("html: {path}");
    }
}

fn parse_repo_input(input: &str) -> Result<RepoInput, Envelope> {
    if input.is_empty() || input.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid_repo_input(input));
    }

    let path = if let Some(rest) = input.strip_prefix("https://github.com/") {
        rest
    } else if input.starts_with("http://") || input.starts_with("https://") {
        return Err(invalid_repo_input(input));
    } else {
        input
    };

    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() != 2 || segments.iter().any(|s| s.is_empty()) {
        return Err(invalid_repo_input(input));
    }
    if !valid_owner_segment(segments[0]) || !valid_repo_segment(segments[1]) {
        return Err(invalid_repo_input(input));
    }

    Ok(RepoInput {
        owner: segments[0].to_string(),
        repo: segments[1].to_string(),
    })
}

fn valid_owner_segment(owner: &str) -> bool {
    !owner.is_empty()
        && !owner.starts_with('-')
        && !owner.ends_with('-')
        && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn valid_repo_segment(repo: &str) -> bool {
    !repo.is_empty()
        && repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn invalid_repo_input(input: &str) -> Envelope {
    Envelope::fail(
        CMD,
        "INVALID_ARGUMENT",
        "repo must be owner/repo or https://github.com/owner/repo",
    )
    .with_details(json!({
        "argument": "repo",
        "value": input,
    }))
}

fn write_outputs_if_requested(
    envelope: Envelope,
    out: Option<&str>,
    html: Option<&str>,
) -> Envelope {
    if !envelope.ok {
        return envelope;
    }
    if out.is_none() && html.is_none() {
        return envelope;
    }

    let mut envelope = envelope;
    if let Some(data) = envelope.data.as_object_mut() {
        if let Some(path) = out {
            data.insert("out".to_string(), json!(path));
        }
        if let Some(path) = html {
            data.insert("html_out".to_string(), json!(path));
        }
    }

    if let Some(path) = html
        && let Err(message) =
            fs::write(path, render_html_report(&envelope.data)).map_err(|err| err.to_string())
    {
        return Envelope::fail(CMD, "OUTPUT_WRITE_FAILED", message).with_details(json!({
            "path": path,
        }));
    }

    if let Some(path) = out {
        match serde_json::to_string_pretty(&envelope)
            .map_err(|err| err.to_string())
            .and_then(|body| fs::write(path, body).map_err(|err| err.to_string()))
        {
            Ok(()) => envelope,
            Err(message) => {
                Envelope::fail(CMD, "OUTPUT_WRITE_FAILED", message).with_details(json!({
                    "path": path,
                }))
            }
        }
    } else {
        envelope
    }
}

fn render_html_report(data: &Value) -> String {
    let owner = text_path(data, &["repository", "owner"], "unknown");
    let repo = text_path(data, &["repository", "repo"], "unknown");
    let html_url = text_path(data, &["repository", "html_url"], "");
    let depth = text_path(data, &["depth"], "unknown");
    let stars = u64_path(data, &["repository", "stars"]).unwrap_or(0);
    let forks = u64_path(data, &["repository", "forks"]).unwrap_or(0);
    let open_issues = u64_path(data, &["repository", "open_issues"]).unwrap_or(0);
    let score = i64_path(data, &["risk", "score"])
        .unwrap_or(0)
        .clamp(0, 100);
    let confidence = f64_path(data, &["risk", "confidence"])
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let evidence_status = text_path(data, &["risk", "evidence_status"], "complete");
    let trust_score = i64_path(data, &["trust", "score"])
        .unwrap_or(100 - score)
        .clamp(0, 100);
    let trust_band = text_path(data, &["trust", "band"], "unknown");
    let reasons = string_array_path(data, &["risk", "reasons"]);
    let evidence = string_array_path(data, &["risk", "evidence"]);
    let sample_requested = u64_path(data, &["sample", "requested"]).unwrap_or(0);
    let sample_fetched = u64_path(data, &["sample", "fetched"]).unwrap_or(0);
    let unavailable = data
        .get("github_api")
        .and_then(|api| api.get("unavailable"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let metrics = vec![
        Metric::score(
            "trust.score",
            trust_score as f64 / 100.0,
            format!("{trust_score}/100"),
        ),
        Metric::score("risk.score", score as f64 / 100.0, format!("{score}/100")),
        Metric::score("risk.confidence", confidence, format!("{confidence:.2}")),
        Metric::share(
            "max_24h_star_share",
            f64_path(data, &["signals", "timeline", "max_24h_star_share"]),
            count_pair(
                u64_path(data, &["signals", "timeline", "max_24h_stars"]),
                u64_path(data, &["signals", "timeline", "starred_at_available_count"]),
            ),
        ),
        Metric::share(
            "max_daily_star_share",
            f64_path(data, &["signals", "timeline", "max_daily_star_share"]),
            count_pair(
                u64_path(data, &["signals", "timeline", "max_daily_stars"]),
                u64_path(data, &["signals", "timeline", "starred_at_available_count"]),
            ),
        ),
        Metric::share(
            "max_hourly_star_share",
            f64_path(data, &["signals", "timeline", "max_hourly_star_share"]),
            count_pair(
                u64_path(data, &["signals", "timeline", "max_hourly_stars"]),
                u64_path(data, &["signals", "timeline", "starred_at_available_count"]),
            ),
        ),
        Metric::share(
            "empty_bio_share",
            f64_path(data, &["signals", "stargazers", "empty_bio_share"]),
            None,
        ),
        Metric::share(
            "low_follower_share",
            f64_path(data, &["signals", "stargazers", "low_follower_share"]),
            None,
        ),
        Metric::share(
            "zero_public_repos_share",
            f64_path(data, &["signals", "stargazers", "zero_public_repos_share"]),
            None,
        ),
        Metric::share(
            "subscriber_star_ratio",
            f64_path(data, &["signals", "repo", "subscriber_star_ratio"]),
            count_pair(
                u64_path(data, &["signals", "repo", "subscribers_count"]),
                Some(stars),
            ),
        ),
        Metric::share(
            "fork_star_ratio",
            f64_path(data, &["signals", "repo", "fork_star_ratio"]),
            Some(format!("{forks}/{stars}")),
        ),
    ];

    let mut out = String::new();
    let title = format!("{owner}/{repo} GitHub Trust Audit");
    write!(
        out,
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{}</title>
<style>
:root {{ --ink:#172018; --muted:#5c685e; --line:#d9e2d6; --paper:#fbfaf4; --card:#ffffff; --accent:#235b3a; --warn:#b7791f; --danger:#9b2c2c; }}
body {{ margin:0; background:linear-gradient(135deg,#f8f4e8 0%,#edf5ec 58%,#dfece2 100%); color:var(--ink); font-family: ui-serif, Georgia, Cambria, "Times New Roman", serif; }}
main {{ max-width:1120px; margin:0 auto; padding:48px 24px 72px; }}
.hero {{ display:grid; grid-template-columns:1.2fr .8fr; gap:24px; align-items:stretch; }}
.card {{ background:rgba(255,255,255,.86); border:1px solid var(--line); border-radius:24px; padding:24px; box-shadow:0 18px 50px rgba(34,61,37,.10); }}
.eyebrow {{ color:var(--accent); font:700 12px/1.2 ui-sans-serif, system-ui; letter-spacing:.14em; text-transform:uppercase; }}
h1 {{ font-size:48px; line-height:1; margin:10px 0 18px; letter-spacing:-.04em; }}
h2 {{ font-size:28px; margin:40px 0 14px; letter-spacing:-.02em; }}
p {{ font-size:18px; line-height:1.55; }}
.score {{ font:800 72px/1 ui-sans-serif, system-ui; letter-spacing:-.06em; }}
.band {{ display:inline-block; padding:8px 12px; border-radius:999px; background:#f4e3bd; color:#6d4308; font:700 13px/1 ui-sans-serif, system-ui; text-transform:uppercase; }}
.kpis {{ display:grid; grid-template-columns:repeat(4,1fr); gap:12px; margin-top:18px; }}
.kpi {{ border:1px solid var(--line); border-radius:16px; padding:14px; background:#fff; }}
.kpi b {{ display:block; font:800 22px/1.1 ui-sans-serif, system-ui; }}
.kpi span {{ color:var(--muted); font:700 12px/1.2 ui-sans-serif, system-ui; text-transform:uppercase; letter-spacing:.08em; }}
.chart {{ margin-top:18px; background:#fff; border:1px solid var(--line); border-radius:18px; padding:16px; overflow:auto; }}
table {{ width:100%; border-collapse:collapse; background:#fff; border-radius:18px; overflow:hidden; }}
th,td {{ border-bottom:1px solid var(--line); padding:12px; text-align:left; vertical-align:top; }}
th {{ font:800 12px/1.2 ui-sans-serif, system-ui; color:var(--muted); text-transform:uppercase; letter-spacing:.08em; }}
code {{ font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }}
.note {{ border-left:4px solid var(--warn); padding:12px 16px; background:#fff8e8; border-radius:12px; }}
@media (max-width: 820px) {{ .hero,.kpis {{ grid-template-columns:1fr; }} h1 {{ font-size:38px; }} }}
</style>
</head>
<body><main>
"#,
        html_escape(&title)
    )
    .unwrap();

    write!(
        out,
        r#"<section class="hero">
<div class="card">
<div class="eyebrow">GitHub Trust Audit</div>
<h1>{}/{}</h1>
<p>This report is a deterministic scorecard for repository trust signals. It measures star-growth concentration, sampled stargazer quality signals, repository ratios, GitHub-native stats availability, and confidence. <strong>Not a fake/real verdict.</strong></p>
<p class="note"><strong>partial evidence means some GitHub-native checks are incomplete</strong>: the trust score is still shown, but confidence is reduced until those checks resolve.</p>
</div>
<div class="card">
<div class="eyebrow">Trust score</div>
<div class="score trust-score-value">{trust_score}</div>
<div class="band">{}</div>
<p>Risk score: <strong class="risk-score-value">{score}</strong></p>
<p>Confidence: <strong class="confidence-value">{confidence:.2}</strong></p>
<p>Evidence status: <strong>{}</strong></p>
<p>Depth: <strong>{}</strong></p>
</div>
</section>
"#,
        html_escape(owner),
        html_escape(repo),
        html_escape(trust_band),
        html_escape(evidence_status),
        html_escape(depth)
    )
    .unwrap();

    write!(
        out,
        r#"<section class="kpis">
<div class="kpi"><span>Stars</span><b>{stars}</b></div>
<div class="kpi"><span>Forks</span><b>{forks}</b></div>
<div class="kpi"><span>Open issues</span><b>{open_issues}</b></div>
<div class="kpi"><span>Sample</span><b>{sample_fetched}/{sample_requested}</b></div>
</section>
<section>
<h2>Metric Dashboard</h2>
<div class="chart">{}</div>
</section>
"#,
        render_metric_svg(&metrics)
    )
    .unwrap();

    out.push_str("<section><h2>Measured Signals</h2><table><thead><tr><th>Metric</th><th>Value</th><th>Observed count</th><th>Why it matters</th></tr></thead><tbody>");
    for metric in &metrics {
        write!(
            out,
            "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td></tr>",
            html_escape(metric.key),
            html_escape(&metric.display_value()),
            html_escape(metric.detail.as_deref().unwrap_or("n/a")),
            html_escape(metric_explanation(metric.key))
        )
        .unwrap();
    }
    out.push_str("</tbody></table></section>");

    out.push_str(
        "<section><h2>Risk Reasons</h2><table><thead><tr><th>Reason</th></tr></thead><tbody>",
    );
    if reasons.is_empty() {
        out.push_str("<tr><td>none</td></tr>");
    } else {
        for reason in reasons {
            write!(
                out,
                "<tr><td><code>{}</code></td></tr>",
                html_escape(&reason)
            )
            .unwrap();
        }
    }
    out.push_str("</tbody></table></section>");

    out.push_str("<section><h2>Evidence And Gaps</h2><table><thead><tr><th>Type</th><th>Value</th><th>Status</th></tr></thead><tbody>");
    for item in evidence {
        write!(
            out,
            "<tr><td>evidence</td><td><code>{}</code></td><td>used</td></tr>",
            html_escape(&item)
        )
        .unwrap();
    }
    for item in unavailable {
        let endpoint = item
            .get("endpoint")
            .or_else(|| item.get("path"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let reason = item
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unavailable");
        let status = item
            .get("status")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string());
        write!(
            out,
            "<tr><td>gap</td><td><code>{}</code></td><td>{} ({})</td></tr>",
            html_escape(endpoint),
            html_escape(reason),
            html_escape(&status)
        )
        .unwrap();
    }
    out.push_str("</tbody></table></section>");

    if !html_url.is_empty() {
        write!(
            out,
            r#"<p>Repository: <a href="{}">{}</a></p>"#,
            attr_escape(html_url),
            html_escape(html_url)
        )
        .unwrap();
    }
    out.push_str("</main></body></html>");
    out
}

struct Metric {
    key: &'static str,
    value: Option<f64>,
    detail: Option<String>,
    score_style: bool,
}

impl Metric {
    fn score(key: &'static str, value: f64, detail: String) -> Self {
        Self {
            key,
            value: Some(value),
            detail: Some(detail),
            score_style: true,
        }
    }

    fn share(key: &'static str, value: Option<f64>, detail: Option<String>) -> Self {
        Self {
            key,
            value,
            detail,
            score_style: false,
        }
    }

    fn display_value(&self) -> String {
        match (self.value, self.score_style) {
            (Some(value), true) => self.detail.clone().unwrap_or_else(|| format!("{value:.2}")),
            (Some(value), false) => format!("{value:.4} ({:.1}%)", value * 100.0),
            (None, _) => "n/a".to_string(),
        }
    }
}

fn render_metric_svg(metrics: &[Metric]) -> String {
    let row_h = 36;
    let width = 940;
    let height = 42 + metrics.len() as i32 * row_h;
    let mut svg = String::new();
    write!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" role="img" aria-label="GitHub trust audit metrics">"#
    )
    .unwrap();
    svg.push_str(r##"<rect width="940" height="100%" rx="14" fill="#fbfaf4"/>"##);
    svg.push_str(r##"<text x="22" y="28" font-family="ui-sans-serif, system-ui" font-size="16" font-weight="800" fill="#172018">GitHub trust audit metrics</text>"##);
    for (idx, metric) in metrics.iter().enumerate() {
        let y = 54 + idx as i32 * row_h;
        let value = metric.value.unwrap_or(0.0).clamp(0.0, 1.0);
        let bar_w = (value * 420.0).round() as i32;
        let color = if metric.score_style {
            "#235b3a"
        } else if value >= 0.6 {
            "#9b2c2c"
        } else if value >= 0.35 {
            "#b7791f"
        } else {
            "#235b3a"
        };
        write!(
            svg,
            r##"<text x="22" y="{y}" font-family="ui-monospace, SFMono-Regular, Menlo, Consolas, monospace" font-size="13" fill="#172018">{}</text>"##,
            html_escape(metric.key)
        )
        .unwrap();
        write!(
            svg,
            r##"<rect x="300" y="{}" width="420" height="16" rx="8" fill="#e4eadf"/><rect x="300" y="{}" width="{bar_w}" height="16" rx="8" fill="{color}"/>"##,
            y - 13,
            y - 13
        )
        .unwrap();
        write!(
            svg,
            r##"<text x="740" y="{y}" font-family="ui-sans-serif, system-ui" font-size="13" font-weight="700" fill="#172018">{}</text>"##,
            html_escape(&metric.display_value())
        )
        .unwrap();
    }
    svg.push_str("</svg>");
    svg
}

fn metric_explanation(key: &str) -> &'static str {
    match key {
        "risk.score" => {
            "Composite deterministic risk score, 0 is lowest observed risk and 100 is highest."
        }
        "trust.score" => "Human-facing repository trust score, 100 is highest observed trust.",
        "risk.confidence" => {
            "How complete the evidence chain is; low confidence means re-run or collect missing stats."
        }
        "max_24h_star_share" => "Large share of stars in one 24h window is a burst signal.",
        "max_daily_star_share" => {
            "Large share of stars on one calendar day is a growth-concentration signal."
        }
        "max_hourly_star_share" => "Large share of stars in one hour is an acute burst signal.",
        "empty_bio_share" => {
            "High share of sampled accounts without bios can indicate low-quality stargazer population."
        }
        "low_follower_share" => {
            "High share of accounts with <=1 follower can indicate weak account credibility."
        }
        "zero_public_repos_share" => {
            "High share of accounts without public repos can indicate low organic developer activity."
        }
        "subscriber_star_ratio" => {
            "Low subscriber-to-star ratio suggests stars are not matched by real watchers."
        }
        "fork_star_ratio" => {
            "Low fork-to-star ratio suggests stars are not matched by downstream developer use."
        }
        _ => "Measured audit signal.",
    }
}

fn text_path<'a>(value: &'a Value, path: &[&str], default: &'a str) -> &'a str {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(Value::as_str)
        .unwrap_or(default)
}

fn f64_path(value: &Value, path: &[&str]) -> Option<f64> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(Value::as_f64)
}

fn u64_path(value: &Value, path: &[&str]) -> Option<u64> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(Value::as_u64)
}

fn i64_path(value: &Value, path: &[&str]) -> Option<i64> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(Value::as_i64)
}

fn string_array_path(value: &Value, path: &[&str]) -> Vec<String> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn count_pair(count: Option<u64>, total: Option<u64>) -> Option<String> {
    match (count, total) {
        (Some(count), Some(total)) => Some(format!("{count}/{total}")),
        _ => None,
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn attr_escape(s: &str) -> String {
    html_escape(s)
}

fn collect_repo_depth(repo: &RepoInput) -> Envelope {
    collect_repo_depth_with_auth(repo, false)
}

fn collect_repo_depth_with_auth(repo: &RepoInput, authenticated: bool) -> Envelope {
    let repo_path = format!("/repos/{}/{}", repo.owner, repo.repo);
    let contributors_path = format!("{repo_path}/contributors?per_page=100");
    let subscribers_path = format!("{repo_path}/subscribers?per_page=100");
    let commit_activity_path = format!("{repo_path}/stats/commit_activity");
    let stats_contributors_path = format!("{repo_path}/stats/contributors");

    let repo_response = match github_get_required(&repo_path, authenticated, GITHUB_JSON_ACCEPT) {
        Ok(response) => response,
        Err(envelope) => return envelope,
    };
    let contributors_response =
        match github_get_required(&contributors_path, authenticated, GITHUB_JSON_ACCEPT) {
            Ok(response) => response,
            Err(envelope) => return envelope,
        };
    let subscribers_response =
        match github_get_required(&subscribers_path, authenticated, GITHUB_JSON_ACCEPT) {
            Ok(response) => response,
            Err(envelope) => return envelope,
        };
    let commit_activity_response =
        match github_get_stats(&commit_activity_path, authenticated, GITHUB_JSON_ACCEPT) {
            Ok(response) => response,
            Err(envelope) => return envelope,
        };
    let stats_contributors_response =
        match github_get_stats(&stats_contributors_path, authenticated, GITHUB_JSON_ACCEPT) {
            Ok(response) => response,
            Err(envelope) => return envelope,
        };

    let repo_json = match validate_repo_response(&repo_response, repo) {
        Ok(repo_json) => repo_json,
        Err(envelope) => return envelope,
    };
    let contributors_count = match validate_array_response(&contributors_response, "contributors") {
        Ok(count) => count,
        Err(envelope) => return envelope,
    };
    let subscribers_count = match validate_array_response(&subscribers_response, "subscribers") {
        Ok(count) => count,
        Err(envelope) => return envelope,
    };
    let commit_activity_availability = match classify_stats_response(&commit_activity_response) {
        Ok(availability) => availability,
        Err(envelope) => return envelope,
    };
    let stats_contributors_availability =
        match classify_stats_response(&stats_contributors_response) {
            Ok(availability) => availability,
            Err(envelope) => return envelope,
        };

    if let Err(envelope) = validate_stats_response(&commit_activity_response) {
        return envelope;
    }
    if let Err(envelope) = validate_stats_response(&stats_contributors_response) {
        return envelope;
    }

    let mut endpoints = vec![
        endpoint_json(&repo_response.endpoint),
        endpoint_json(&contributors_response.endpoint),
        endpoint_json(&subscribers_response.endpoint),
    ];
    let mut unavailable = Vec::new();
    push_stats_record(
        &mut endpoints,
        &mut unavailable,
        &commit_activity_response.endpoint,
        commit_activity_availability,
    );
    push_stats_record(
        &mut endpoints,
        &mut unavailable,
        &stats_contributors_response.endpoint,
        stats_contributors_availability,
    );
    for path in [
        format!("{repo_path}/traffic/views"),
        format!("{repo_path}/traffic/clones"),
        format!("{repo_path}/traffic/popular/referrers"),
    ] {
        match github_get_optional(&path, authenticated, GITHUB_JSON_ACCEPT) {
            Ok(response) => endpoints.push(endpoint_json(&response.endpoint)),
            Err(record) => unavailable.push(json!({
                "endpoint": record.path.clone(),
                "path": record.path,
                "status": record.status,
                "reason": "unavailable",
            })),
        }
    }

    let commit_activity_source = stats_source(commit_activity_availability);
    let stats_contributors_source = stats_source(stats_contributors_availability);
    let (commit_activity_total_52w, commit_activity_weeks) =
        commit_activity_totals(&commit_activity_response);

    let stars = numeric_field(&repo_json, "stargazers_count").unwrap_or(0);
    let forks = numeric_field(&repo_json, "forks_count").unwrap_or(0);
    let open_issues = numeric_field(&repo_json, "open_issues_count").unwrap_or(0);
    let html_url = repo_json
        .get("html_url")
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("https://github.com/{}/{}", repo.owner, repo.repo));
    let canonical_owner = repo_json
        .get("owner")
        .and_then(|v| v.get("login"))
        .and_then(|v| v.as_str())
        .unwrap_or(&repo.owner);
    let canonical_repo = repo_json
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&repo.repo);

    let mut envelope = Envelope::ok(
        CMD,
        json!({
            "repository": {
                "owner": canonical_owner,
                "repo": canonical_repo,
                "html_url": html_url,
                "stars": stars,
                "forks": forks,
                "open_issues": open_issues,
            },
            "depth": "repo",
            "sample": {
                "requested": 0,
                "fetched": 0,
                "pages": 0,
            },
            "risk": {
                "score": 0,
                "band": "low",
                "confidence": 0.5,
                "reasons": [],
                "evidence": [],
            },
            "signals": {
                "repo": {
                    "stars": stars,
                    "forks": forks,
                    "open_issues": open_issues,
                    "contributors_count": contributors_count,
                    "subscribers_count": subscribers_count,
                    "fork_star_ratio": ratio(forks, stars),
                    "subscriber_star_ratio": ratio(subscribers_count as u64, stars),
                    "issue_star_ratio": ratio(open_issues, stars),
                    "contributors_star_ratio": ratio(contributors_count as u64, stars),
                    "commit_activity_source": commit_activity_source,
                    "stats_contributors_source": stats_contributors_source,
                    "commit_activity_total_52w": commit_activity_total_52w,
                    "commit_activity_weeks": commit_activity_weeks,
                    "commit_activity_per_1k_stars": per_1k_stars(commit_activity_total_52w, stars),
                    "watchers_count_ignored": true,
                },
                "stargazers": {},
                "timeline": {},
            },
            "github_api": {
                "authenticated": authenticated,
                "endpoints": endpoints,
                "unavailable": unavailable,
                "rate_limit_remaining_min": null,
            },
        }),
    );
    if let Some(data) = envelope.data.as_object_mut() {
        update_risk(data);
    }
    envelope
}

fn collect_stargazers_depth(repo: &RepoInput, sample: usize) -> Envelope {
    let mut envelope = collect_repo_depth_with_auth(repo, true);
    if !envelope.ok {
        return with_auth_depth(envelope, "stargazers");
    }

    let stargazers = match collect_stargazers(repo, sample) {
        Ok(stargazers) => stargazers,
        Err(envelope) => return with_auth_depth(envelope, "stargazers"),
    };
    let stargazer_signals = stargazer_signals(&stargazers.profiles);
    let starred_at_available_count = stargazers
        .samples
        .iter()
        .filter(|sample| sample.starred_at.is_some())
        .count();

    if let Some(data) = envelope.data.as_object_mut() {
        data.insert("depth".to_string(), json!("stargazers"));
        data.insert(
            "sample".to_string(),
            json!({
                "requested": sample,
                "fetched": stargazers.samples.len(),
                "pages": stargazers.pages,
            }),
        );
        if let Some(signals) = data.get_mut("signals").and_then(Value::as_object_mut) {
            signals.insert("stargazers".to_string(), stargazer_signals);
            signals.insert(
                "timeline".to_string(),
                json!({
                    "starred_at_available_count": starred_at_available_count,
                }),
            );
        }
        if let Some(github_api) = data.get_mut("github_api").and_then(Value::as_object_mut) {
            github_api.insert("authenticated".to_string(), json!(true));
            if let Some(endpoints) = github_api
                .get_mut("endpoints")
                .and_then(Value::as_array_mut)
            {
                endpoints.extend(stargazers.endpoints);
            }
        }
        update_risk(data);
    }

    envelope
}

fn collect_timeline_depth(repo: &RepoInput, sample: usize) -> Envelope {
    let mut envelope = collect_repo_depth_with_auth(repo, true);
    if !envelope.ok {
        return with_auth_depth(envelope, "timeline");
    }

    let stargazers = match collect_stargazers(repo, sample) {
        Ok(stargazers) => stargazers,
        Err(envelope) => return with_auth_depth(envelope, "timeline"),
    };
    let stargazer_signals = stargazer_signals(&stargazers.profiles);
    let timeline_signals = timeline_signals(&stargazers, sample);

    if let Some(data) = envelope.data.as_object_mut() {
        data.insert("depth".to_string(), json!("timeline"));
        data.insert(
            "sample".to_string(),
            json!({
                "requested": sample,
                "fetched": stargazers.samples.len(),
                "pages": stargazers.pages,
            }),
        );
        if let Some(signals) = data.get_mut("signals").and_then(Value::as_object_mut) {
            signals.insert("stargazers".to_string(), stargazer_signals);
            signals.insert("timeline".to_string(), timeline_signals);
        }
        if let Some(github_api) = data.get_mut("github_api").and_then(Value::as_object_mut) {
            github_api.insert("authenticated".to_string(), json!(true));
            if let Some(endpoints) = github_api
                .get_mut("endpoints")
                .and_then(Value::as_array_mut)
            {
                endpoints.extend(stargazers.endpoints);
            }
        }
        update_risk(data);
    }

    envelope
}

fn collect_stargazers(repo: &RepoInput, sample: usize) -> Result<StargazerCollection, Envelope> {
    let mut samples = Vec::new();
    let mut endpoints = Vec::new();
    let page_count = sample.div_ceil(100);
    let mut pages_fetched = 0;

    for page in 1..=page_count {
        let path = format!(
            "/repos/{}/{}/stargazers?per_page=100&page={page}",
            repo.owner, repo.repo
        );
        let response = github_get_required(&path, true, GITHUB_STAR_ACCEPT)?;
        pages_fetched += 1;
        endpoints.push(endpoint_json(&response.endpoint));

        let page_samples = parse_stargazer_page(&response)?;
        if page_samples.is_empty() {
            break;
        }
        let short_page = page_samples.len() < 100;
        for stargazer in page_samples {
            if samples.len() >= sample {
                break;
            }
            samples.push(stargazer);
        }
        if samples.len() >= sample || short_page {
            break;
        }
    }

    let mut profiles = Vec::new();
    for sample in &samples {
        let path = format!("/users/{}", sample.login);
        let response = github_get_required(&path, true, GITHUB_JSON_ACCEPT)?;
        endpoints.push(endpoint_json(&response.endpoint));
        profiles.push(parse_stargazer_profile(&response)?);
    }

    Ok(StargazerCollection {
        samples,
        profiles,
        pages: pages_fetched,
        endpoints,
    })
}

fn parse_stargazer_page(response: &GithubResponse) -> Result<Vec<StargazerSample>, Envelope> {
    let Some(items) = response.value.as_ref().and_then(|value| value.as_array()) else {
        return Err(invalid_github_shape(
            &response.endpoint,
            "stargazers response must be a JSON array",
        ));
    };

    let mut samples = Vec::new();
    for item in items {
        let starred_at = item
            .get("starred_at")
            .and_then(|value| value.as_str())
            .map(ToString::to_string);
        let login = item
            .get("user")
            .and_then(|user| user.get("login"))
            .and_then(|value| value.as_str())
            .or_else(|| item.get("login").and_then(|value| value.as_str()));
        let Some(login) = login else {
            return Err(invalid_github_shape(
                &response.endpoint,
                "stargazer item must include login",
            ));
        };
        samples.push(StargazerSample {
            login: login.to_string(),
            starred_at,
        });
    }

    Ok(samples)
}

fn parse_stargazer_profile(response: &GithubResponse) -> Result<StargazerProfile, Envelope> {
    let Some(value) = response.value.as_ref() else {
        return Err(invalid_github_shape(
            &response.endpoint,
            "user profile response must be a JSON object",
        ));
    };
    if !value.is_object() {
        return Err(invalid_github_shape(
            &response.endpoint,
            "user profile response must be a JSON object",
        ));
    }

    let created_at = value
        .get("created_at")
        .and_then(|value| value.as_str())
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc));
    let followers = numeric_field(value, "followers").unwrap_or(0);
    let public_repos = numeric_field(value, "public_repos").unwrap_or(0);
    let empty_bio = value
        .get("bio")
        .and_then(|value| value.as_str())
        .map(|bio| bio.trim().is_empty())
        .unwrap_or(true);

    Ok(StargazerProfile {
        created_at,
        followers,
        public_repos,
        empty_bio,
    })
}

fn stargazer_signals(profiles: &[StargazerProfile]) -> Value {
    let total = profiles.len();
    let mut ages: Vec<i64> = profiles
        .iter()
        .filter_map(|profile| profile.created_at)
        .map(|created_at| (Utc::now() - created_at).num_days().max(0))
        .collect();
    ages.sort_unstable();

    let empty_bio_count = profiles.iter().filter(|profile| profile.empty_bio).count();
    let zero_public_repos_count = profiles
        .iter()
        .filter(|profile| profile.public_repos == 0)
        .count();
    let low_follower_count = profiles
        .iter()
        .filter(|profile| profile.followers <= 1)
        .count();
    let zero_follower_count = profiles
        .iter()
        .filter(|profile| profile.followers == 0)
        .count();

    let mut signals = Map::new();
    signals.insert("accounts_sampled".to_string(), json!(total));
    signals.insert(
        "empty_bio_share".to_string(),
        json!(share(empty_bio_count, total)),
    );
    signals.insert(
        "zero_public_repos_share".to_string(),
        json!(share(zero_public_repos_count, total)),
    );
    signals.insert(
        "low_follower_share".to_string(),
        json!(share(low_follower_count, total)),
    );
    signals.insert(
        "zero_follower_share".to_string(),
        json!(share(zero_follower_count, total)),
    );
    if !ages.is_empty() {
        signals.insert(
            "account_age_days_p50".to_string(),
            json!(ages[ages.len() / 2]),
        );
    }

    Value::Object(signals)
}

fn timeline_signals(stargazers: &StargazerCollection, requested: usize) -> Value {
    let mut daily: BTreeMap<String, usize> = BTreeMap::new();
    let mut hourly: BTreeMap<String, usize> = BTreeMap::new();
    let mut starred_times = Vec::new();

    for sample in &stargazers.samples {
        let Some(starred_at) = sample.starred_at.as_deref() else {
            continue;
        };
        let Ok(ts) = DateTime::parse_from_rfc3339(starred_at) else {
            continue;
        };
        let ts = ts.with_timezone(&Utc);
        *daily.entry(ts.format("%Y-%m-%d").to_string()).or_insert(0) += 1;
        *hourly
            .entry(ts.format("%Y-%m-%dT%H:00:00Z").to_string())
            .or_insert(0) += 1;
        starred_times.push(ts);
    }
    starred_times.sort_unstable();

    let available = starred_times.len();
    let max_daily_stars = daily.values().copied().max().unwrap_or(0);
    let max_hourly_stars = hourly.values().copied().max().unwrap_or(0);
    let max_24h_stars = max_window_count(&starred_times, 24 * 60 * 60);

    let mut created_dates: BTreeMap<String, usize> = BTreeMap::new();
    for profile in &stargazers.profiles {
        if let Some(created_at) = profile.created_at {
            *created_dates
                .entry(created_at.format("%Y-%m-%d").to_string())
                .or_insert(0) += 1;
        }
    }
    let max_creation_date_count = created_dates.values().copied().max().unwrap_or(0);

    json!({
        "starred_at_available_count": available,
        "starred_at_coverage": share(available, stargazers.samples.len()),
        "sample_coverage": share(stargazers.samples.len(), requested),
        "max_daily_stars": max_daily_stars,
        "max_daily_star_share": share(max_daily_stars, available),
        "max_hourly_stars": max_hourly_stars,
        "max_hourly_star_share": share(max_hourly_stars, available),
        "max_burst_window_hours": 24,
        "max_burst_window_stars": max_24h_stars,
        "max_burst_window_star_share": share(max_24h_stars, available),
        "max_24h_stars": max_24h_stars,
        "max_24h_star_share": share(max_24h_stars, available),
        "account_creation_date_max_count": max_creation_date_count,
        "account_creation_date_max_share": share(max_creation_date_count, stargazers.profiles.len()),
    })
}

fn max_window_count(times: &[DateTime<Utc>], window_seconds: i64) -> usize {
    let mut max_count = 0;
    let mut end = 0;
    for start in 0..times.len() {
        while end < times.len() && (times[end] - times[start]).num_seconds() <= window_seconds {
            end += 1;
        }
        max_count = max_count.max(end - start);
    }
    max_count
}

fn update_risk(data: &mut Map<String, Value>) {
    let Some(signals) = data.get("signals") else {
        return;
    };

    let mut score = 0;
    let mut reasons = Vec::new();
    let mut evidence = Vec::new();
    let mut unknown_reasons = Vec::new();

    add_low_share_risk(
        signals,
        &mut score,
        &mut reasons,
        &mut evidence,
        "repo",
        "fork_star_ratio",
        &[(0.001, 20), (0.005, 10)],
    );
    add_low_share_risk(
        signals,
        &mut score,
        &mut reasons,
        &mut evidence,
        "repo",
        "subscriber_star_ratio",
        &[(0.0002, 20), (0.001, 10)],
    );
    add_low_share_risk(
        signals,
        &mut score,
        &mut reasons,
        &mut evidence,
        "repo",
        "contributors_star_ratio",
        &[(0.0002, 20), (0.001, 10)],
    );
    add_high_share_risk(
        signals,
        &mut score,
        &mut reasons,
        &mut evidence,
        "repo",
        "issue_star_ratio",
        &[(0.10, 8), (0.25, 15)],
    );
    add_low_commit_activity_risk(data, &mut score, &mut reasons, &mut evidence);

    let commit_activity_source = signals
        .get("repo")
        .and_then(|repo| repo.get("commit_activity_source"))
        .and_then(Value::as_str);
    if matches!(commit_activity_source, Some("stats_pending")) {
        unknown_reasons.push("github_stats_pending".to_string());
    } else if !matches!(commit_activity_source, Some("github_native_stats")) {
        unknown_reasons.push("github_stats_unavailable".to_string());
    }
    let stats_contributors_source = signals
        .get("repo")
        .and_then(|repo| repo.get("stats_contributors_source"))
        .and_then(Value::as_str);
    if matches!(stats_contributors_source, Some("stats_pending")) {
        push_unique_reason(&mut unknown_reasons, "github_stats_pending");
    } else if !matches!(stats_contributors_source, Some("github_native_stats")) {
        push_unique_reason(&mut unknown_reasons, "github_stats_unavailable");
    }

    add_share_risk(
        signals,
        &mut score,
        &mut reasons,
        &mut evidence,
        "stargazers",
        "low_follower_share",
        &[(0.70, 20), (0.40, 10)],
    );
    add_share_risk(
        signals,
        &mut score,
        &mut reasons,
        &mut evidence,
        "stargazers",
        "zero_follower_share",
        &[(0.50, 15), (0.25, 8)],
    );
    add_share_risk(
        signals,
        &mut score,
        &mut reasons,
        &mut evidence,
        "stargazers",
        "zero_public_repos_share",
        &[(0.50, 15), (0.25, 8)],
    );
    // Empty bios are common on otherwise legitimate GitHub accounts, so this
    // remains a displayed context metric rather than a scoring penalty.
    add_share_risk(
        signals,
        &mut score,
        &mut reasons,
        &mut evidence,
        "timeline",
        "max_daily_star_share",
        &[(0.80, 20), (0.60, 12), (0.35, 5)],
    );
    add_share_risk(
        signals,
        &mut score,
        &mut reasons,
        &mut evidence,
        "timeline",
        "max_hourly_star_share",
        &[(0.50, 25), (0.25, 12)],
    );
    add_share_risk(
        signals,
        &mut score,
        &mut reasons,
        &mut evidence,
        "timeline",
        "max_24h_star_share",
        &[(0.90, 15), (0.70, 5), (0.35, 3)],
    );
    if reasons.iter().any(|reason| {
        reason.starts_with("max_daily_star_share=")
            || reason.starts_with("max_hourly_star_share=")
            || reason.starts_with("max_24h_star_share=")
    }) && !reasons.iter().any(|reason| reason == "star_burst")
    {
        reasons.push("star_burst".to_string());
        evidence.push("signals.timeline".to_string());
    }
    add_share_risk(
        signals,
        &mut score,
        &mut reasons,
        &mut evidence,
        "timeline",
        "account_creation_date_max_share",
        &[(0.60, 15), (0.35, 8)],
    );

    let fetched = data
        .get("sample")
        .and_then(|sample| sample.get("fetched"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let requested = data
        .get("sample")
        .and_then(|sample| sample.get("requested"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if requested > 0 && fetched < requested.min(20) {
        unknown_reasons.push("insufficient_sample".to_string());
    }

    if is_uncorroborated_star_burst(&reasons) {
        score = score.min(10);
        push_unique_reason(&mut reasons, "uncorroborated_star_burst_cap");
    }

    score = score.min(100);
    let band = if score >= 70 {
        "high"
    } else if score >= 30 {
        "medium"
    } else {
        "low"
    };
    let mut confidence = if fetched >= 100 {
        0.8
    } else if fetched >= 20 {
        0.65
    } else if fetched > 0 {
        0.45
    } else {
        0.5
    };
    let evidence_status = if unknown_reasons.is_empty() {
        "complete"
    } else {
        confidence = 0.35;
        for reason in unknown_reasons {
            if !reasons.iter().any(|existing| existing == &reason) {
                reasons.push(reason);
            }
        }
        "partial"
    };
    let trust_score = (100 - score).clamp(0, 100);
    let trust_band = if trust_score >= 80 {
        "high"
    } else if trust_score >= 60 {
        "medium"
    } else {
        "low"
    };

    data.insert(
        "risk".to_string(),
        json!({
            "score": score,
            "band": band,
            "confidence": confidence,
            "evidence_status": evidence_status,
            "reasons": reasons,
            "evidence": evidence,
        }),
    );
    data.insert(
        "trust".to_string(),
        json!({
            "score": trust_score,
            "band": trust_band,
            "confidence": confidence,
            "evidence_status": evidence_status,
        }),
    );
}

fn is_uncorroborated_star_burst(reasons: &[String]) -> bool {
    let has_burst = reasons.iter().any(|reason| {
        reason == "star_burst"
            || reason.starts_with("max_daily_star_share=")
            || reason.starts_with("max_hourly_star_share=")
            || reason.starts_with("max_24h_star_share=")
    });
    let has_corroboration = reasons.iter().any(|reason| {
        reason.starts_with("fork_star_ratio=")
            || reason.starts_with("subscriber_star_ratio=")
            || reason.starts_with("contributors_star_ratio=")
            || reason.starts_with("issue_star_ratio=")
            || reason.starts_with("low_commit_activity_per_star=")
            || reason.starts_with("low_follower_share=")
            || reason.starts_with("zero_follower_share=")
            || reason.starts_with("zero_public_repos_share=")
            || reason.starts_with("account_creation_date_max_share=")
    });

    has_burst && !has_corroboration
}

fn add_share_risk(
    signals: &Value,
    score: &mut i32,
    reasons: &mut Vec<String>,
    evidence: &mut Vec<String>,
    section: &str,
    key: &str,
    thresholds: &[(f64, i32)],
) {
    let Some(value) = signals
        .get(section)
        .and_then(|section| section.get(key))
        .and_then(Value::as_f64)
    else {
        return;
    };

    for (threshold, points) in thresholds {
        if value >= *threshold {
            *score += *points;
            reasons.push(format!("{key}={value:.2}"));
            evidence.push(format!("signals.{section}.{key}"));
            break;
        }
    }
}

fn add_low_share_risk(
    signals: &Value,
    score: &mut i32,
    reasons: &mut Vec<String>,
    evidence: &mut Vec<String>,
    section: &str,
    key: &str,
    thresholds: &[(f64, i32)],
) {
    let Some(value) = signals
        .get(section)
        .and_then(|section| section.get(key))
        .and_then(Value::as_f64)
    else {
        return;
    };

    for (threshold, points) in thresholds {
        if value <= *threshold {
            *score += *points;
            reasons.push(format!("{key}={value:.4}"));
            evidence.push(format!("signals.{section}.{key}"));
            break;
        }
    }
}

fn add_high_share_risk(
    signals: &Value,
    score: &mut i32,
    reasons: &mut Vec<String>,
    evidence: &mut Vec<String>,
    section: &str,
    key: &str,
    thresholds: &[(f64, i32)],
) {
    let Some(value) = signals
        .get(section)
        .and_then(|section| section.get(key))
        .and_then(Value::as_f64)
    else {
        return;
    };

    for (threshold, points) in thresholds.iter().rev() {
        if value >= *threshold {
            *score += *points;
            reasons.push(format!("{key}={value:.4}"));
            evidence.push(format!("signals.{section}.{key}"));
            break;
        }
    }
}

fn add_low_commit_activity_risk(
    data: &Map<String, Value>,
    score: &mut i32,
    reasons: &mut Vec<String>,
    evidence: &mut Vec<String>,
) {
    let Some(commits_per_1k_stars) = data
        .get("signals")
        .and_then(|signals| signals.get("repo"))
        .and_then(|repo| repo.get("commit_activity_per_1k_stars"))
        .and_then(Value::as_f64)
    else {
        return;
    };
    let stars = data
        .get("signals")
        .and_then(|signals| signals.get("repo"))
        .and_then(|repo| repo.get("stars"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let source = data
        .get("signals")
        .and_then(|signals| signals.get("repo"))
        .and_then(|repo| repo.get("commit_activity_source"))
        .and_then(Value::as_str);

    if stars >= 1000 && source == Some("github_native_stats") && commits_per_1k_stars < 2.0 {
        *score += 20;
        reasons.push(format!(
            "low_commit_activity_per_star={commits_per_1k_stars:.2}"
        ));
        evidence.push(
            endpoint_for(data, "/stats/commit_activity")
                .unwrap_or_else(|| "signals.repo.commit_activity_total_52w".to_string()),
        );
    }
}

fn endpoint_for(data: &Map<String, Value>, suffix: &str) -> Option<String> {
    data.get("github_api")
        .and_then(|api| api.get("endpoints"))
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|item| {
            item.get("endpoint")
                .or_else(|| item.get("path"))
                .and_then(Value::as_str)
        })
        .find(|endpoint| endpoint.ends_with(suffix))
        .map(ToString::to_string)
}

fn push_unique_reason(reasons: &mut Vec<String>, reason: &str) {
    if !reasons.iter().any(|existing| existing == reason) {
        reasons.push(reason.to_string());
    }
}

fn share(count: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        count as f64 / total as f64
    }
}

fn ratio(count: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        count as f64 / total as f64
    }
}

fn per_1k_stars(count: u64, stars: u64) -> f64 {
    ratio(count, stars) * 1000.0
}

fn commit_activity_totals(response: &GithubResponse) -> (u64, usize) {
    let Some(weeks) = response.value.as_ref().and_then(Value::as_array) else {
        return (0, 0);
    };
    let total = weeks
        .iter()
        .filter_map(|week| numeric_field(week, "total"))
        .sum();
    (total, weeks.len())
}

fn github_get_required(
    path: &str,
    authenticated: bool,
    accept: &str,
) -> Result<GithubResponse, Envelope> {
    let response = github_get(path, authenticated, accept).map_err(|err| match err {
        GithubFetchError::CredentialRequired => github_token_required_without_depth(),
        GithubFetchError::Other(message) => {
            Envelope::fail(CMD, "FETCH_FAILED", message).with_details(json!({ "path": path }))
        }
    })?;

    if response.endpoint.status != Some(200) {
        return Err(
            Envelope::fail(CMD, "GITHUB_API_ERROR", "GitHub API request failed").with_details(
                json!({
                    "path": response.endpoint.path,
                    "status": response.endpoint.status,
                }),
            ),
        );
    }

    if response.value.is_none() {
        return Err(
            Envelope::fail(CMD, "GITHUB_API_ERROR", "GitHub API response was not JSON")
                .with_details(json!({
                    "path": response.endpoint.path,
                    "status": response.endpoint.status,
                })),
        );
    }

    Ok(response)
}

fn github_get_stats(
    path: &str,
    authenticated: bool,
    accept: &str,
) -> Result<GithubResponse, Envelope> {
    github_get(path, authenticated, accept).map_err(|err| match err {
        GithubFetchError::CredentialRequired => github_token_required_without_depth(),
        GithubFetchError::Other(message) => {
            Envelope::fail(CMD, "FETCH_FAILED", message).with_details(json!({ "path": path }))
        }
    })
}

fn github_get_optional(
    path: &str,
    authenticated: bool,
    accept: &str,
) -> Result<GithubResponse, EndpointRecord> {
    match github_get(path, authenticated, accept) {
        Ok(response) if response.endpoint.status == Some(200) && response.value.is_some() => {
            Ok(response)
        }
        Ok(response) => Err(response.endpoint),
        Err(_) => Err(EndpointRecord {
            path: path.to_string(),
            status: None,
            body_bytes: 0,
        }),
    }
}

fn github_get(
    path: &str,
    authenticated: bool,
    accept: &str,
) -> Result<GithubResponse, GithubFetchError> {
    let url = format!("{GITHUB_API}{path}");
    let mut args = vec![
        "send".to_string(),
        url,
        "-H".to_string(),
        format!("Accept: {accept}"),
    ];
    if authenticated {
        args.push("-H".to_string());
        args.push(POSTAGENT_GITHUB_AUTH_HEADER.to_string());
    }
    let raw = postagent::run_args(&args, FETCH_TIMEOUT_MS).map_err(GithubFetchError::Other)?;
    if authenticated && postagent_github_credential_error(&raw.raw_stderr) {
        return Err(GithubFetchError::CredentialRequired);
    }

    let parsed = postagent::parse(&raw)
        .ok_or_else(|| GithubFetchError::Other("parse postagent output".to_string()))?;
    let value = if parsed.status == Some(200) && parsed.body_non_empty {
        serde_json::from_slice(&raw.raw_stdout).ok()
    } else {
        None
    };

    Ok(GithubResponse {
        endpoint: EndpointRecord {
            path: path.to_string(),
            status: parsed.status,
            body_bytes: parsed.body_bytes,
        },
        value,
    })
}

fn postagent_github_credential_error(stderr: &[u8]) -> bool {
    let stderr = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    let mentions_github_placeholder =
        stderr.contains("postagent.github.token") || stderr.contains("github.token");
    let mentions_auth_config = stderr.contains("credential")
        || stderr.contains("token")
        || stderr.contains("placeholder")
        || stderr.contains("auth config")
        || stderr.contains("authorization");

    mentions_github_placeholder && mentions_auth_config
}

fn github_token_required(depth: &str) -> Envelope {
    Envelope::fail(
        CMD,
        "GITHUB_TOKEN_REQUIRED",
        "GitHub credential is required for this depth",
    )
    .with_details(json!({
        "depth": depth,
        "sub_code": "GITHUB_TOKEN_REQUIRED",
    }))
}

fn github_token_required_without_depth() -> Envelope {
    Envelope::fail(
        CMD,
        "GITHUB_TOKEN_REQUIRED",
        "GitHub credential is required for this depth",
    )
    .with_details(json!({
        "sub_code": "GITHUB_TOKEN_REQUIRED",
    }))
}

fn with_auth_depth(mut envelope: Envelope, depth: &str) -> Envelope {
    if envelope
        .error
        .as_ref()
        .is_some_and(|err| err.code == "GITHUB_TOKEN_REQUIRED")
    {
        envelope = github_token_required(depth);
    }
    envelope
}

fn endpoint_json(record: &EndpointRecord) -> Value {
    json!({
        "endpoint": record.path.clone(),
        "path": record.path.clone(),
        "status": record.status,
        "body_bytes": record.body_bytes,
    })
}

fn unavailable_json(record: &EndpointRecord, reason: &str) -> Value {
    json!({
        "endpoint": record.path.clone(),
        "path": record.path.clone(),
        "status": record.status,
        "reason": reason,
    })
}

fn push_stats_record(
    endpoints: &mut Vec<Value>,
    unavailable: &mut Vec<Value>,
    record: &EndpointRecord,
    availability: StatsAvailability,
) {
    match availability {
        StatsAvailability::Available => endpoints.push(endpoint_json(record)),
        StatsAvailability::Pending => unavailable.push(unavailable_json(record, "stats_pending")),
        StatsAvailability::Unavailable => unavailable.push(unavailable_json(record, "unavailable")),
    }
}

fn stats_source(availability: StatsAvailability) -> &'static str {
    match availability {
        StatsAvailability::Available => "github_native_stats",
        StatsAvailability::Pending => "stats_pending",
        StatsAvailability::Unavailable => "unavailable",
    }
}

fn validate_repo_response(response: &GithubResponse, repo: &RepoInput) -> Result<Value, Envelope> {
    let Some(value) = response.value.as_ref() else {
        return Err(invalid_github_shape(
            &response.endpoint,
            "repository response must be a JSON object",
        ));
    };
    if !value.is_object() {
        return Err(invalid_github_shape(
            &response.endpoint,
            "repository response must be a JSON object",
        ));
    }
    for field in ["stargazers_count", "forks_count", "open_issues_count"] {
        if numeric_field(value, field).is_none() {
            return Err(invalid_github_shape(
                &response.endpoint,
                format!("repository field {field} must be numeric"),
            ));
        }
    }

    let owner = value
        .get("owner")
        .and_then(|v| v.get("login"))
        .and_then(|v| v.as_str())
        .unwrap_or(&repo.owner);
    let name = value
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&repo.repo);
    if !owner.eq_ignore_ascii_case(&repo.owner) || !name.eq_ignore_ascii_case(&repo.repo) {
        return Err(invalid_github_shape(
            &response.endpoint,
            "repository identity did not match requested owner/repo",
        ));
    }

    Ok(value.clone())
}

fn validate_array_response(response: &GithubResponse, name: &str) -> Result<usize, Envelope> {
    match response.value.as_ref().and_then(|v| v.as_array()) {
        Some(items) => Ok(items.len()),
        None => Err(invalid_github_shape(
            &response.endpoint,
            format!("{name} response must be a JSON array"),
        )),
    }
}

fn classify_stats_response(response: &GithubResponse) -> Result<StatsAvailability, Envelope> {
    match response.endpoint.status {
        Some(200) => {
            if response.value.as_ref().is_some_and(|v| v.is_array()) {
                Ok(StatsAvailability::Available)
            } else {
                Ok(StatsAvailability::Pending)
            }
        }
        Some(202) => Ok(StatsAvailability::Pending),
        _ => Ok(StatsAvailability::Unavailable),
    }
}

fn validate_stats_response(response: &GithubResponse) -> Result<(), Envelope> {
    classify_stats_response(response).map(|_| ())
}

fn invalid_github_shape(record: &EndpointRecord, message: impl Into<String>) -> Envelope {
    Envelope::fail(CMD, "GITHUB_API_ERROR", message).with_details(json!({
        "path": record.path,
        "status": record.status,
    }))
}

fn numeric_field(value: &Value, field: &str) -> Option<u64> {
    value.get(field).and_then(|v| v.as_u64())
}
