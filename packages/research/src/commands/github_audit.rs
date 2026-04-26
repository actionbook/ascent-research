use serde_json::json;

use crate::output::{Envelope, not_implemented};

const CMD: &str = "research github-audit";
const DEPTHS: &[&str] = &["repo", "stargazers", "timeline"];

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

    let _ = (repo, out);
    not_implemented(CMD)
}
