//! Minimal markdown section parser for session.md.
//!
//! We only care about ATX `##` headings as section boundaries, and `###`
//! sub-headings inside `## Findings` as one-finding-per-heading. No full
//! CommonMark parser needed — the input is templated + LLM-edited and the
//! structure is uniform. This avoids an extra dependency.

use std::collections::HashMap;

/// Convenience: pull out the `## Overview` section body from a session.md,
/// or None if missing / empty / just placeholder.
pub fn extract_overview(md: &str) -> Option<String> {
    let sections = parse_sections(md);
    let body = sections.get("Overview")?.trim();
    if body.is_empty() {
        return None;
    }
    // Placeholder-only (a single HTML comment) shouldn't propagate.
    if body.starts_with("<!--") && body.ends_with("-->") && !body.contains('\n') {
        return None;
    }
    Some(body.to_string())
}

/// Parse top-level `## <name>` sections. Returns a map of section name to
/// body text (without the heading line itself; trimmed).
pub fn parse_sections(md: &str) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    let mut current: Option<String> = None;
    let mut buf = String::new();
    for line in md.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            // Flush previous section.
            if let Some(name) = current.take() {
                out.insert(name, buf.trim().to_string());
            }
            current = Some(rest.trim().to_string());
            buf.clear();
        } else if current.is_some() {
            buf.push_str(line);
            buf.push('\n');
        }
        // lines before the first `## ` heading are ignored (e.g. H1 title)
    }
    if let Some(name) = current.take() {
        out.insert(name, buf.trim().to_string());
    }
    out
}

/// Represents one finding parsed from the `## Findings` section.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub title: String,
    pub body: String,
}

/// Parse `### Heading\nbody...` blocks inside a Findings section body.
pub fn parse_findings(section_body: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut current_title: Option<String> = None;
    let mut buf = String::new();
    for line in section_body.lines() {
        if let Some(rest) = line.strip_prefix("### ") {
            if let Some(title) = current_title.take() {
                let body = buf.trim().to_string();
                if !title.is_empty() {
                    out.push(Finding { title, body });
                }
            }
            current_title = Some(rest.trim().to_string());
            buf.clear();
        } else if current_title.is_some() {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    if let Some(title) = current_title {
        let body = buf.trim().to_string();
        if !title.is_empty() {
            out.push(Finding { title, body });
        }
    }
    out
}

/// Parse simple `- label: value [suffix]` metric lines.
#[derive(Debug, Clone, PartialEq)]
pub struct Metric {
    pub label: String,
    pub value: String,
    pub suffix: Option<String>,
}

/// Extract unique `http(s)://…` URLs that appear inside markdown
/// `[text](url)` link syntax. Used by `research diff` + `research coverage`
/// to compare "cited in body" against `source_accepted` events.
///
/// If `exclude_sources_block` is true, content between the CLI-maintained
/// markers `<!-- research:sources-start -->` and `<!-- research:sources-end -->`
/// is stripped before scanning — that block is a cache, not narrative, so
/// cited URLs there don't count toward "body citations".
pub fn extract_http_links(md: &str, exclude_sources_block: bool) -> Vec<String> {
    let scanned: String = if exclude_sources_block {
        strip_sources_block(md)
    } else {
        md.to_string()
    };

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let bytes = scanned.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Look for "](http" which is the start of a markdown-link URL.
        if bytes[i] == b']' && i + 1 < bytes.len() && bytes[i + 1] == b'(' {
            let start = i + 2;
            // Match the closing `)` that balances the opening `(`, so URLs
            // like `https://en.wikipedia.org/wiki/Function_(mathematics)`
            // survive instead of getting truncated at the first `)`. A
            // markdown link may optionally carry a " title" after the URL
            // (e.g. `[t](url "cap")`); we stop on the first unquoted space
            // outside nested parens.
            let tail = &scanned[start..];
            let mut depth: i32 = 1;
            let mut in_quotes: Option<u8> = None;
            let mut end_rel: Option<usize> = None;
            let tail_bytes = tail.as_bytes();
            for (k, &b) in tail_bytes.iter().enumerate() {
                match (b, in_quotes) {
                    (b'"', None) => in_quotes = Some(b'"'),
                    (b'\'', None) => in_quotes = Some(b'\''),
                    (q, Some(open)) if q == open => in_quotes = None,
                    (b'(', None) => depth += 1,
                    (b')', None) => {
                        depth -= 1;
                        if depth == 0 {
                            end_rel = Some(k);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if let Some(end_rel) = end_rel {
                let raw = &scanned[start..start + end_rel];
                // Split off optional `"title"` / `'title'` portion.
                let url_part = raw.trim().split_whitespace().next().unwrap_or(raw.trim());
                let url = url_part.trim();
                if url.starts_with("http://") || url.starts_with("https://") {
                    if seen.insert(url.to_string()) {
                        out.push(url.to_string());
                    }
                }
                i = start + end_rel + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn strip_sources_block(md: &str) -> String {
    let start_marker = "<!-- research:sources-start -->";
    let end_marker = "<!-- research:sources-end -->";
    let Some(s) = md.find(start_marker) else {
        return md.to_string();
    };
    let after_start = s + start_marker.len();
    let Some(e_rel) = md[after_start..].find(end_marker) else {
        return md.to_string();
    };
    let e = after_start + e_rel + end_marker.len();
    let mut out = String::with_capacity(md.len());
    out.push_str(&md[..s]);
    out.push_str(&md[e..]);
    out
}

pub fn parse_metrics(section_body: &str) -> Vec<Metric> {
    let mut out = Vec::new();
    for line in section_body.lines() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) else {
            continue;
        };
        let Some((label, tail)) = rest.split_once(':') else {
            continue;
        };
        let tail = tail.trim();
        // `NN suffix` or just `NN`
        let (value, suffix) = match tail.split_once(' ') {
            Some((v, s)) => (v.trim().to_string(), Some(s.trim().to_string())),
            None => (tail.to_string(), None),
        };
        out.push(Metric {
            label: label.trim().to_string(),
            value,
            suffix,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Research: Topic

## Overview
Overview body.

## Findings
### Finding A
Body for A.

### Finding B
Body for B.

## Metrics
- Throughput: 1.5 req/s
- Count: 42

## Notes
Long notes here.
";

    #[test]
    fn sections_are_parsed() {
        let m = parse_sections(SAMPLE);
        assert!(m.contains_key("Overview"));
        assert!(m.contains_key("Findings"));
        assert!(m.contains_key("Metrics"));
        assert!(m.contains_key("Notes"));
        assert_eq!(m["Overview"], "Overview body.");
    }

    #[test]
    fn findings_parsed() {
        let m = parse_sections(SAMPLE);
        let findings = parse_findings(&m["Findings"]);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].title, "Finding A");
        assert_eq!(findings[0].body, "Body for A.");
        assert_eq!(findings[1].title, "Finding B");
    }

    #[test]
    fn metrics_parsed() {
        let m = parse_sections(SAMPLE);
        let metrics = parse_metrics(&m["Metrics"]);
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[0].label, "Throughput");
        assert_eq!(metrics[0].value, "1.5");
        assert_eq!(metrics[0].suffix.as_deref(), Some("req/s"));
        assert_eq!(metrics[1].suffix, None);
    }

    #[test]
    fn missing_section_returns_none() {
        let md = "## Only\nbody\n";
        let m = parse_sections(md);
        assert!(!m.contains_key("Overview"));
    }

    #[test]
    fn extract_http_links_finds_inline_refs() {
        let md = "See [A](https://a.test/) and also [B](http://b.test/x).\n\nNot a link: plain text.\n";
        let mut links = extract_http_links(md, false);
        links.sort();
        assert_eq!(
            links,
            vec!["http://b.test/x".to_string(), "https://a.test/".to_string()]
        );
    }

    #[test]
    fn extract_http_links_skips_non_http_schemes() {
        let md = "[a](mailto:x@y) [b](ftp://host) [c](/local/path) [ok](https://ok.test)";
        let links = extract_http_links(md, false);
        assert_eq!(links, vec!["https://ok.test"]);
    }

    #[test]
    fn extract_http_links_dedupes() {
        let md = "[x](https://a.test) and again [y](https://a.test)";
        let mut links = extract_http_links(md, false);
        links.sort();
        assert_eq!(links, vec!["https://a.test"]);
    }

    #[test]
    fn extract_http_links_can_exclude_sources_block() {
        let md = "Body: [a](https://real.test).\n\n## Sources\n<!-- research:sources-start -->\n- [k · trust 2.0] https://cache.test/\n<!-- research:sources-end -->\n\n## Findings\n[x](https://deeper.test)";
        let without = extract_http_links(md, true);
        assert!(without.iter().any(|u| u == "https://real.test"));
        assert!(without.iter().any(|u| u == "https://deeper.test"));
        // cache.test is only inside the sources block — excluded
        assert!(!without.iter().any(|u| u == "https://cache.test/"));

        let with = extract_http_links(md, false);
        // When we don't exclude, plain-text URL in sources block still isn't
        // a markdown link syntactically, so it stays out. But anything like
        // [a](url) inside the block would be caught.
        assert!(with.iter().any(|u| u == "https://real.test"));
    }

    #[test]
    fn extract_http_links_preserves_urls_with_parens() {
        let md = "See [wiki](https://en.wikipedia.org/wiki/Function_(mathematics)) for details.";
        let links = extract_http_links(md, false);
        assert_eq!(
            links,
            vec!["https://en.wikipedia.org/wiki/Function_(mathematics)".to_string()]
        );
    }

    #[test]
    fn extract_http_links_handles_title_attribute() {
        let md = r#"Check [x](https://example.com/path "the title") here."#;
        let links = extract_http_links(md, false);
        assert_eq!(links, vec!["https://example.com/path".to_string()]);
    }

    #[test]
    fn extract_http_links_handles_nested_parens_with_title() {
        let md = r#"See [y](https://en.wikipedia.org/wiki/Rust_(programming_language) "Rust lang")."#;
        let links = extract_http_links(md, false);
        assert_eq!(
            links,
            vec!["https://en.wikipedia.org/wiki/Rust_(programming_language)".to_string()]
        );
    }
}
