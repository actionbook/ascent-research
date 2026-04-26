use chrono::{DateTime, Utc};
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::env;

use crate::fetch::postagent;
use crate::output::{Envelope, not_implemented};

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
}

pub fn run(repo: &str, depth: &str, sample: usize, out: Option<&str>) -> Envelope {
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

    if matches!(depth, "stargazers" | "timeline") && !postagent_github_token_available() {
        return Envelope::fail(
            CMD,
            "GITHUB_TOKEN_REQUIRED",
            "GitHub credential is required for this depth",
        )
        .with_details(json!({ "depth": depth }));
    }

    if depth == "timeline" {
        let _ = out;
        return not_implemented(CMD);
    }

    if depth == "stargazers" {
        let _ = out;
        collect_stargazers_depth(&repo_input, sample)
    } else {
        collect_repo_depth(&repo_input)
    }
}

fn postagent_github_token_available() -> bool {
    env::var("POSTAGENT_GITHUB_TOKEN_AVAILABLE")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
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

    let commit_activity_source = if commit_activity_availability == StatsAvailability::Available {
        "github_native_stats"
    } else if commit_activity_availability == StatsAvailability::Pending {
        "stats_pending"
    } else {
        "unavailable"
    };

    let stars = numeric_field(&repo_json, "stargazers_count").unwrap_or(0);
    let forks = numeric_field(&repo_json, "forks_count").unwrap_or(0);
    let open_issues = numeric_field(&repo_json, "open_issues_count").unwrap_or(0);
    let html_url = repo_json
        .get("html_url")
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("https://github.com/{}/{}", repo.owner, repo.repo));

    Envelope::ok(
        CMD,
        json!({
            "repository": {
                "owner": repo.owner,
                "repo": repo.repo,
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
            },
            "signals": {
                "repo": {
                    "stars": stars,
                    "forks": forks,
                    "open_issues": open_issues,
                    "contributors_count": contributors_count,
                    "subscribers_count": subscribers_count,
                    "commit_activity_source": commit_activity_source,
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
    )
}

fn collect_stargazers_depth(repo: &RepoInput, sample: usize) -> Envelope {
    let mut envelope = collect_repo_depth_with_auth(repo, true);
    if !envelope.ok {
        return envelope;
    }

    let stargazers = match collect_stargazers(repo, sample) {
        Ok(stargazers) => stargazers,
        Err(envelope) => return envelope,
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
        let response = match github_get_required(&path, true, GITHUB_STAR_ACCEPT) {
            Ok(response) => response,
            Err(envelope) => return Err(envelope),
        };
        pages_fetched += 1;
        endpoints.push(endpoint_json(&response.endpoint));

        let page_samples = match parse_stargazer_page(&response) {
            Ok(page_samples) => page_samples,
            Err(envelope) => return Err(envelope),
        };
        for stargazer in page_samples {
            if samples.len() >= sample {
                break;
            }
            samples.push(stargazer);
        }
        if samples.len() >= sample {
            break;
        }
    }

    let mut profiles = Vec::new();
    for sample in &samples {
        let path = format!("/users/{}", sample.login);
        let response = match github_get_required(&path, true, GITHUB_JSON_ACCEPT) {
            Ok(response) => response,
            Err(envelope) => return Err(envelope),
        };
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

fn share(count: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        count as f64 / total as f64
    }
}

fn github_get_required(
    path: &str,
    authenticated: bool,
    accept: &str,
) -> Result<GithubResponse, Envelope> {
    let response = github_get(path, authenticated, accept).map_err(|message| {
        Envelope::fail(CMD, "FETCH_FAILED", message).with_details(json!({ "path": path }))
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
    let response = github_get(path, authenticated, accept).map_err(|message| {
        Envelope::fail(CMD, "FETCH_FAILED", message).with_details(json!({ "path": path }))
    })?;

    if matches!(response.endpoint.status, Some(200) | Some(202)) {
        Ok(response)
    } else {
        Err(
            Envelope::fail(CMD, "GITHUB_API_ERROR", "GitHub stats API request failed")
                .with_details(json!({
                    "path": response.endpoint.path,
                    "status": response.endpoint.status,
                })),
        )
    }
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

fn github_get(path: &str, authenticated: bool, accept: &str) -> Result<GithubResponse, String> {
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
    let raw = postagent::run_args(&args, FETCH_TIMEOUT_MS)?;
    let parsed = postagent::parse(&raw).ok_or_else(|| "parse postagent output".to_string())?;
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
    if owner != repo.owner || name != repo.repo {
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
        _ => Err(
            Envelope::fail(CMD, "GITHUB_API_ERROR", "GitHub stats API request failed")
                .with_details(json!({
                    "path": response.endpoint.path,
                    "status": response.endpoint.status,
                })),
        ),
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
