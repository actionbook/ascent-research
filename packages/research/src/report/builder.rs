//! Build a json-ui Report document from parsed session state.
//!
//! Canonical 8 sections (per research-synthesize.spec.md):
//! 1. BrandHeader
//! 2. Section "Overview" — Prose
//! 3. Section "Key Findings" — ContributionList
//! 4. (optional) Section "Metrics" — MetricsGrid
//! 5. Section "Analysis" — Prose from Notes
//! 6. (optional) Section "Conclusion" — Prose
//! 7. Section "Sources" — LinkGroup
//! 8. Section "Methodology" — Callout
//! + BrandFooter

use chrono::Utc;
use regex::Regex;
use serde_json::{Value, json};
use std::sync::OnceLock;

use crate::session::{
    event::SessionEvent,
    md_parser::{self, Finding, Metric},
};

/// Input bundle describing everything needed to build a report.
pub struct ReportInput<'a> {
    pub topic: &'a str,
    pub preset: &'a str,
    pub md: &'a str,
    pub events: &'a [SessionEvent],
}

#[derive(Debug, Clone, PartialEq)]
pub enum BuildError {
    MissingOverview,
}

pub struct ReportBuild {
    pub json: Value,
    pub accepted_count: u32,
    pub rejected_count: u32,
    pub executor_breakdown: ExecutorBreakdown,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ExecutorBreakdown {
    pub postagent: u32,
    pub browser: u32,
}

pub fn build(input: &ReportInput) -> Result<ReportBuild, BuildError> {
    let sections = md_parser::parse_sections(input.md);
    let overview = sections
        .get("Overview")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && !looks_like_placeholder(s))
        .ok_or(BuildError::MissingOverview)?;

    let mut children: Vec<Value> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // 1. BrandHeader
    children.push(json!({
        "type": "BrandHeader",
        "props": {
            "badge": "Research Report",
            "poweredBy": "Actionbook / research CLI"
        }
    }));

    // 2. Overview
    children.push(section("Overview", "paper", vec![prose(overview)]));

    // 3. Key Findings — only render when the session actually has
    // `### Title` children under `## Findings`. The numbered sections
    // (## 01 · TITLE …) are the agent's preferred surface; we leave the
    // Findings section optional so autoresearch reports don't ship a
    // "no findings recorded" stub block.
    let findings: Vec<Finding> = sections
        .get("Findings")
        .map(|s| md_parser::parse_findings(s))
        .unwrap_or_default();
    if findings.is_empty() {
        warnings.push("`## Findings` empty — skipped (numbered sections carry the content)".into());
    } else {
        let items: Vec<Value> = findings
            .iter()
            .enumerate()
            .map(|(i, f)| {
                json!({
                    "badge": format!("{}", i + 1),
                    "title": f.title,
                    "description": f.body
                })
            })
            .collect();
        children.push(section(
            "Key Findings",
            "star",
            vec![json!({
                "type": "ContributionList",
                "props": { "items": items }
            })],
        ));
    }

    // 4. Metrics (optional)
    if let Some(body) = sections.get("Metrics") {
        let metrics: Vec<Metric> = md_parser::parse_metrics(body);
        if !metrics.is_empty() {
            let entries: Vec<Value> = metrics
                .iter()
                .map(|m| {
                    let mut entry = json!({
                        "label": m.label,
                        "value": m.value,
                    });
                    if let Some(s) = &m.suffix {
                        entry["suffix"] = json!(s);
                    } else {
                        entry["suffix"] = json!("");
                    }
                    entry
                })
                .collect();
            children.push(section(
                "Metrics",
                "chart",
                vec![json!({
                    "type": "MetricsGrid",
                    "props": { "metrics": entries, "cols": 3 }
                })],
            ));
        }
    }

    // 5. Analysis (Notes section)
    if let Some(body) = sections.get("Notes")
        && !body.trim().is_empty()
        && !looks_like_placeholder(body)
    {
        children.push(section("Analysis", "bulb", vec![prose(body)]));
    }

    // 5b. Numbered content sections (## 01 · TITLE … ## 06 · TITLE) — the
    // autoresearch loop writes substantive prose + diagram references
    // here, and the builder used to drop all of it because it only knew
    // about the legacy Findings / Notes / Conclusion template sections.
    let mut numbered: Vec<(u32, String, String)> = Vec::new();
    for (name, body) in &sections {
        if let Some((num, title)) = parse_numbered_heading(name)
            && !body.trim().is_empty()
            && !looks_like_placeholder(body)
        {
            numbered.push((num, title, body.clone()));
        }
    }
    numbered.sort_by_key(|(n, _, _)| *n);
    for (num, title, body) in numbered {
        let display_title = format!("{num:02} · {title}");
        let section_children = split_body_on_diagrams(&body);
        children.push(section(&display_title, "paper", section_children));
    }

    // 6. Conclusion (optional)
    if let Some(body) = sections.get("Conclusion")
        && !body.trim().is_empty()
    {
        children.push(section("Conclusion", "info", vec![prose(body)]));
    }

    // 7. Sources + gather stats
    let mut accepted_count = 0u32;
    let mut rejected_count = 0u32;
    let mut breakdown = ExecutorBreakdown::default();
    let mut links: Vec<Value> = Vec::new();
    for ev in input.events {
        match ev {
            SessionEvent::SourceAccepted {
                url,
                kind,
                executor,
                trust_score,
                ..
            } => {
                accepted_count += 1;
                match executor.as_str() {
                    "postagent" => breakdown.postagent += 1,
                    "browser" => breakdown.browser += 1,
                    _ => {}
                }
                links.push(json!({
                    "href": url,
                    "label": format!("[{kind} · trust {trust_score:.1}] {url}"),
                    "icon": match executor.as_str() {
                        "postagent" => "code",
                        _ => "book",
                    }
                }));
            }
            SessionEvent::SourceRejected { .. } => {
                rejected_count += 1;
            }
            _ => {}
        }
    }
    children.push(section(
        "Sources",
        "link",
        vec![json!({
            "type": "LinkGroup",
            "props": { "links": links }
        })],
    ));

    // 8. Methodology — structured data fields so tests don't rely on string match.
    let methodology_text = format!(
        "Total accepted: {accepted} (postagent: {pa}, browser: {br}) · Rejected: {rj} · Preset: {preset}",
        accepted = accepted_count,
        pa = breakdown.postagent,
        br = breakdown.browser,
        rj = rejected_count,
        preset = input.preset,
    );
    children.push(json!({
        "type": "Section",
        "props": { "title": "Methodology", "icon": "info" },
        "children": [
            {
                "type": "Callout",
                "props": {
                    "type": "note",
                    "title": "Source inventory",
                    "content": methodology_text,
                    "data": {
                        "accepted_total": accepted_count,
                        "accepted_postagent": breakdown.postagent,
                        "accepted_browser": breakdown.browser,
                        "rejected_total": rejected_count,
                        "preset": input.preset,
                    }
                }
            }
        ]
    }));

    // BrandFooter
    children.push(json!({
        "type": "BrandFooter",
        "props": {
            "timestamp": Utc::now().to_rfc3339(),
            "attribution": "Powered by Actionbook + postagent",
            "disclaimer": "Generated by the research CLI. Verify critical claims against upstream sources."
        }
    }));

    let root = json!({
        "type": "Report",
        "props": { "theme": "auto" },
        "children": children,
    });

    Ok(ReportBuild {
        json: root,
        accepted_count,
        rejected_count,
        executor_breakdown: breakdown,
        warnings,
    })
}

fn section(title: &str, icon: &str, children: Vec<Value>) -> Value {
    json!({
        "type": "Section",
        "props": { "title": title, "icon": icon },
        "children": children,
    })
}

fn prose(content: &str) -> Value {
    json!({
        "type": "Prose",
        "props": { "content": content }
    })
}

fn looks_like_placeholder(s: &str) -> bool {
    let t = s.trim();
    // Heuristic: only HTML-comment placeholder or very short.
    (t.starts_with("<!--") && t.ends_with("-->")) || t.len() < 10
}

/// Split a section body at markdown diagram references, returning a mixed
/// list of Prose + Image child nodes. Each `![alt](diagrams/foo.svg)`
/// becomes its own Image node (which json-ui renders as `<img>`); the
/// surrounding text stays as Prose. Prose's markdown parser escapes raw
/// HTML, so the only reliable way to embed an image is via Image nodes.
fn split_body_on_diagrams(body: &str) -> Vec<Value> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"!\[([^\]]*)\]\((diagrams/[^)\s]+\.svg)\)").expect("diagram img regex")
    });
    let mut out: Vec<Value> = Vec::new();
    let mut last_end = 0usize;
    for caps in re.captures_iter(body) {
        let m = caps.get(0).unwrap();
        let before = &body[last_end..m.start()];
        if !before.trim().is_empty() {
            out.push(prose(before.trim()));
        }
        let alt = caps.get(1).map(|mm| mm.as_str()).unwrap_or("");
        let src = caps.get(2).map(|mm| mm.as_str()).unwrap_or("");
        out.push(json!({
            "type": "Image",
            "props": {
                "src": src,
                "alt": alt,
                "width": "100%"
            }
        }));
        last_end = m.end();
    }
    let tail = &body[last_end..];
    if !tail.trim().is_empty() {
        out.push(prose(tail.trim()));
    }
    if out.is_empty() {
        out.push(prose(body));
    }
    out
}

/// Parse a `## NN · TITLE` style heading (just the heading text, without
/// the leading `## `). Returns `(num, title)` if it matches, else None.
fn parse_numbered_heading(name: &str) -> Option<(u32, String)> {
    let mut iter = name.splitn(2, '·');
    let num = iter.next()?.trim().parse::<u32>().ok()?;
    let title = iter.next()?.trim().to_string();
    if title.is_empty() {
        return None;
    }
    Some((num, title))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn sample_md() -> &'static str {
        "\
# Research: Topic

## Overview
Overview body in two sentences. Good enough.

## Findings
### A
Body A.

### B
Body B.

## Notes
Analytical prose here.
"
    }

    fn empty_events() -> Vec<SessionEvent> {
        Vec::new()
    }

    #[test]
    fn missing_overview_errors() {
        let md = "## Findings\n### A\nbody\n";
        let r = build(&ReportInput {
            topic: "T",
            preset: "tech",
            md,
            events: &empty_events(),
        });
        assert_eq!(r.err(), Some(BuildError::MissingOverview));
    }

    #[test]
    fn placeholder_overview_treated_as_missing() {
        let md = "## Overview\n<!-- fill me -->\n";
        assert_eq!(
            build(&ReportInput {
                topic: "T",
                preset: "tech",
                md,
                events: &empty_events(),
            })
            .err(),
            Some(BuildError::MissingOverview)
        );
    }

    #[test]
    fn numbered_sections_are_rendered_in_order() {
        let md = "\
## Overview
Agent pipeline for fetching and digesting. Enough text for the placeholder test.

## 03 · HOW
loop architecture body

## 01 · WHY
problem framing body

## 02 · WHAT
taxonomy body with a diagram reference ![fig](diagrams/axis.svg) inline.
";
        let out = build(&ReportInput {
            topic: "T",
            preset: "tech",
            md,
            events: &empty_events(),
        })
        .unwrap();
        let titles: Vec<&str> = out.json["children"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["props"]["title"].as_str())
            .collect();
        // Numbered sections must appear, sorted by their leading number.
        let why_idx = titles.iter().position(|t| *t == "01 · WHY").unwrap();
        let what_idx = titles.iter().position(|t| *t == "02 · WHAT").unwrap();
        let how_idx = titles.iter().position(|t| *t == "03 · HOW").unwrap();
        assert!(why_idx < what_idx && what_idx < how_idx);
        // Diagram markdown reference must reach the Prose content verbatim —
        // the json-ui renderer is responsible for turning ![](…) into <img>.
        let what_section = out.json["children"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["props"]["title"] == "02 · WHAT")
            .unwrap();
        // Section has been split into Prose + Image + Prose. The middle
        // (or later) child must be an Image node pointing at the SVG.
        let what_children = what_section["children"].as_array().unwrap();
        let has_image = what_children
            .iter()
            .any(|c| c["type"] == "Image" && c["props"]["src"] == "diagrams/axis.svg");
        assert!(
            has_image,
            "expected an Image child with src=diagrams/axis.svg; got:\n{what_children:#?}"
        );
        // No Prose child should still carry the raw markdown image syntax.
        for c in what_children {
            if c["type"] == "Prose" {
                let content = c["props"]["content"].as_str().unwrap_or("");
                assert!(
                    !content.contains("![fig]"),
                    "prose block must not retain the raw markdown image syntax"
                );
            }
        }
    }

    #[test]
    fn empty_notes_section_is_skipped() {
        let md = "\
## Overview
Real overview content that passes the placeholder heuristic cleanly.

## Notes
<!-- free-form prose; become the Detailed Analysis section -->
";
        let out = build(&ReportInput {
            topic: "T",
            preset: "tech",
            md,
            events: &empty_events(),
        })
        .unwrap();
        let titles: Vec<&str> = out.json["children"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["props"]["title"].as_str())
            .collect();
        assert!(
            !titles.contains(&"Analysis"),
            "placeholder Notes body must not render as Analysis — got {titles:?}"
        );
    }

    #[test]
    fn findings_render_as_contribution_list() {
        let out = build(&ReportInput {
            topic: "T",
            preset: "tech",
            md: sample_md(),
            events: &empty_events(),
        })
        .unwrap();
        let children = out.json["children"].as_array().unwrap();
        let findings_section = children
            .iter()
            .find(|c| c["props"]["title"] == "Key Findings")
            .unwrap();
        let list = &findings_section["children"][0];
        assert_eq!(list["type"], "ContributionList");
        assert_eq!(list["props"]["items"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn empty_findings_is_skipped_not_stubbed() {
        let md = "\
## Overview
Real overview content that passes the placeholder heuristic cleanly.

## Findings
<!-- `### Title` + body, one heading per finding -->
";
        let out = build(&ReportInput {
            topic: "T",
            preset: "tech",
            md,
            events: &empty_events(),
        })
        .unwrap();
        let titles: Vec<&str> = out.json["children"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["props"]["title"].as_str())
            .collect();
        assert!(
            !titles.contains(&"Key Findings"),
            "empty Findings must drop the block entirely; got {titles:?}"
        );
    }

    #[test]
    fn methodology_counts_are_structured() {
        let events = vec![
            SessionEvent::SourceAccepted {
                timestamp: Utc::now(),
                url: "https://a".into(),
                kind: "hn-item".into(),
                executor: "postagent".into(),
                raw_path: "raw/1.json".into(),
                bytes: 100,
                trust_score: 2.0,
                note: None,
            },
            SessionEvent::SourceAccepted {
                timestamp: Utc::now(),
                url: "https://b".into(),
                kind: "browser-fallback".into(),
                executor: "browser".into(),
                raw_path: "raw/2.json".into(),
                bytes: 800,
                trust_score: 1.0,
                note: None,
            },
            SessionEvent::SourceRejected {
                timestamp: Utc::now(),
                url: "https://c".into(),
                kind: "k".into(),
                executor: "browser".into(),
                reason: crate::session::event::RejectReason::WrongUrl,
                observed_url: None,
                observed_bytes: None,
                rejected_raw_path: None,
                note: None,
            },
        ];
        let out = build(&ReportInput {
            topic: "T",
            preset: "tech",
            md: sample_md(),
            events: &events,
        })
        .unwrap();
        assert_eq!(out.accepted_count, 2);
        assert_eq!(out.rejected_count, 1);
        assert_eq!(out.executor_breakdown.postagent, 1);
        assert_eq!(out.executor_breakdown.browser, 1);

        let children = out.json["children"].as_array().unwrap();
        let m = children
            .iter()
            .find(|c| c["props"]["title"] == "Methodology")
            .unwrap();
        let data = &m["children"][0]["props"]["data"];
        assert_eq!(data["accepted_total"], 2);
        assert_eq!(data["accepted_postagent"], 1);
        assert_eq!(data["accepted_browser"], 1);
        assert_eq!(data["rejected_total"], 1);
        assert_eq!(data["preset"], "tech");
    }

    #[test]
    fn sources_section_skips_rejected() {
        let events = vec![
            SessionEvent::SourceAccepted {
                timestamp: Utc::now(),
                url: "https://a".into(),
                kind: "hn-item".into(),
                executor: "postagent".into(),
                raw_path: "raw/1.json".into(),
                bytes: 100,
                trust_score: 2.0,
                note: None,
            },
            SessionEvent::SourceRejected {
                timestamp: Utc::now(),
                url: "https://b".into(),
                kind: "k".into(),
                executor: "browser".into(),
                reason: crate::session::event::RejectReason::WrongUrl,
                observed_url: None,
                observed_bytes: None,
                rejected_raw_path: None,
                note: None,
            },
        ];
        let out = build(&ReportInput {
            topic: "T",
            preset: "tech",
            md: sample_md(),
            events: &events,
        })
        .unwrap();
        let children = out.json["children"].as_array().unwrap();
        let s = children
            .iter()
            .find(|c| c["props"]["title"] == "Sources")
            .unwrap();
        let links = s["children"][0]["props"]["links"].as_array().unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0]["href"], "https://a");
    }
}
