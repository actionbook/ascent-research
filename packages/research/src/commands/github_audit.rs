use serde_json::Value;
use serde_json::json;

use crate::fetch::postagent;
use crate::output::{Envelope, not_implemented};

const CMD: &str = "research github-audit";
const DEPTHS: &[&str] = &["repo", "stargazers", "timeline"];
const GITHUB_API: &str = "https://api.github.com";
const FETCH_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone)]
struct RepoInput {
    owner: String,
    repo: String,
}

struct GithubResponse {
    endpoint: EndpointRecord,
    value: Option<Value>,
}

#[derive(Clone)]
struct EndpointRecord {
    path: String,
    status: Option<i32>,
    body_bytes: u64,
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

    if depth != "repo" {
        let _ = out;
        return not_implemented(CMD);
    }

    collect_repo_depth(&repo_input)
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
    let repo_path = format!("/repos/{}/{}", repo.owner, repo.repo);
    let contributors_path = format!("{repo_path}/contributors?per_page=100");
    let subscribers_path = format!("{repo_path}/subscribers?per_page=100");
    let commit_activity_path = format!("{repo_path}/stats/commit_activity");
    let stats_contributors_path = format!("{repo_path}/stats/contributors");

    let repo_response = match github_get_required(&repo_path) {
        Ok(response) => response,
        Err(envelope) => return envelope,
    };
    let contributors_response = match github_get_required(&contributors_path) {
        Ok(response) => response,
        Err(envelope) => return envelope,
    };
    let subscribers_response = match github_get_required(&subscribers_path) {
        Ok(response) => response,
        Err(envelope) => return envelope,
    };
    let commit_activity_response = match github_get_stats(&commit_activity_path) {
        Ok(response) => response,
        Err(envelope) => return envelope,
    };
    let stats_contributors_response = match github_get_stats(&stats_contributors_path) {
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
    let commit_activity_available = match validate_stats_response(&commit_activity_response) {
        Ok(available) => available,
        Err(envelope) => return envelope,
    };
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
    );
    push_stats_record(
        &mut endpoints,
        &mut unavailable,
        &stats_contributors_response.endpoint,
    );
    for path in [
        format!("{repo_path}/traffic/views"),
        format!("{repo_path}/traffic/clones"),
        format!("{repo_path}/traffic/popular/referrers"),
    ] {
        match github_get_optional(&path) {
            Ok(response) => endpoints.push(endpoint_json(&response.endpoint)),
            Err(record) => unavailable.push(json!({
                "endpoint": record.path.clone(),
                "path": record.path,
                "status": record.status,
                "reason": "unavailable",
            })),
        }
    }

    let commit_activity_source = if commit_activity_available {
        "github_native_stats"
    } else if commit_activity_response.endpoint.status == Some(202) {
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
                "authenticated": false,
                "endpoints": endpoints,
                "unavailable": unavailable,
                "rate_limit_remaining_min": null,
            },
        }),
    )
}

fn github_get_required(path: &str) -> Result<GithubResponse, Envelope> {
    let response = github_get(path).map_err(|message| {
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

fn github_get_stats(path: &str) -> Result<GithubResponse, Envelope> {
    let response = github_get(path).map_err(|message| {
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

fn github_get_optional(path: &str) -> Result<GithubResponse, EndpointRecord> {
    match github_get(path) {
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

fn github_get(path: &str) -> Result<GithubResponse, String> {
    let url = format!("{GITHUB_API}{path}");
    let args = vec![
        "send".to_string(),
        url,
        "-H".to_string(),
        "Accept: application/vnd.github+json".to_string(),
    ];
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
) {
    if record.status == Some(200) {
        endpoints.push(endpoint_json(record));
    } else {
        let reason = if record.status == Some(202) {
            "stats_pending"
        } else {
            "unavailable"
        };
        unavailable.push(unavailable_json(record, reason));
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

fn validate_stats_response(response: &GithubResponse) -> Result<bool, Envelope> {
    match response.endpoint.status {
        Some(200) => {
            if response.value.as_ref().is_some_and(|v| v.is_array()) {
                Ok(true)
            } else {
                Err(invalid_github_shape(
                    &response.endpoint,
                    "stats response must be a JSON array",
                ))
            }
        }
        Some(202) => Ok(false),
        _ => Err(
            Envelope::fail(CMD, "GITHUB_API_ERROR", "GitHub stats API request failed")
                .with_details(json!({
                    "path": response.endpoint.path,
                    "status": response.endpoint.status,
                })),
        ),
    }
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
