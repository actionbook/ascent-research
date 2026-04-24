//! Generate the Sources `<ul>` for the rich report.
//!
//! Reads `session.jsonl`, keeps only `source_accepted` events, sorts by
//! timestamp ascending, and emits an HTML list. The session.md sources block
//! is **not** consulted — jsonl is the authoritative fact stream, md is a
//! human-readable projection.

use crate::session::event::{SessionEvent, read_events};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct SourcesSection {
    pub html: String,
    pub count: u32,
    pub total_bytes: u64,
    pub warnings: Vec<String>,
}

pub fn build_from_jsonl(jsonl_path: &Path) -> SourcesSection {
    let mut accepted: Vec<AcceptedView> = Vec::new();

    match read_events(jsonl_path) {
        Ok(events) => {
            for ev in events {
                if let SessionEvent::SourceAccepted {
                    timestamp,
                    url,
                    kind,
                    bytes,
                    ..
                } = ev
                {
                    accepted.push(AcceptedView {
                        ts: timestamp.to_rfc3339(),
                        kind,
                        url,
                        bytes,
                    });
                }
            }
        }
        Err(_) => {
            // No jsonl or unreadable — the CLI wrapper treats this as empty.
        }
    }

    accepted.sort_by(|a, b| a.ts.cmp(&b.ts));

    let mut warnings = Vec::new();
    if accepted.is_empty() {
        warnings.push("no_sources".to_string());
        return SourcesSection {
            html: "<p><em>(no sources accepted yet in this session)</em></p>".to_string(),
            count: 0,
            total_bytes: 0,
            warnings,
        };
    }

    let total_bytes: u64 = accepted.iter().map(|a| a.bytes).sum();
    let count = accepted.len() as u32;

    let mut html = String::from("<ul>\n");
    for a in &accepted {
        html.push_str("  <li>");
        html.push_str(&format!(
            "<span class=\"kind\">{}</span><a href=\"{}\">{}</a>",
            html_escape(&a.kind),
            attr_escape(&a.url),
            html_escape(&a.url),
        ));
        html.push_str("</li>\n");
    }
    html.push_str("</ul>\n");

    SourcesSection {
        html,
        count,
        total_bytes,
        warnings,
    }
}

struct AcceptedView {
    ts: String,
    kind: String,
    url: String,
    bytes: u64,
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn attr_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_jsonl(lines: &[&str]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        f.flush().unwrap();
        f
    }

    #[test]
    fn empty_jsonl_yields_no_sources_warning() {
        let tmp = NamedTempFile::new().unwrap();
        let section = build_from_jsonl(tmp.path());
        assert_eq!(section.count, 0);
        assert!(section.warnings.contains(&"no_sources".to_string()));
        assert!(section.html.contains("no sources accepted"));
    }

    #[test]
    fn non_existent_file_yields_no_sources_warning() {
        let section = build_from_jsonl(Path::new("/tmp/__does_not_exist_12345.jsonl"));
        assert_eq!(section.count, 0);
        assert!(section.warnings.contains(&"no_sources".to_string()));
    }

    #[test]
    fn accepted_sources_rendered_in_order() {
        let f = write_jsonl(&[
            r#"{"event":"session_created","timestamp":"2026-04-19T10:00:00Z","slug":"s","topic":"t","preset":"tech","session_dir_abs":"/tmp"}"#,
            r#"{"event":"source_accepted","timestamp":"2026-04-19T10:01:00Z","url":"https://example.com/a","kind":"github-file","executor":"postagent","raw_path":"raw/1.json","bytes":100,"trust_score":2.0}"#,
            r#"{"event":"source_accepted","timestamp":"2026-04-19T10:02:00Z","url":"https://example.com/b","kind":"github-tree","executor":"postagent","raw_path":"raw/2.json","bytes":200,"trust_score":2.0}"#,
        ]);
        let section = build_from_jsonl(f.path());
        assert_eq!(section.count, 2);
        assert_eq!(section.total_bytes, 300);
        assert!(section.warnings.is_empty());
        // Order preserved (a before b)
        let pos_a = section.html.find("example.com/a").unwrap();
        let pos_b = section.html.find("example.com/b").unwrap();
        assert!(pos_a < pos_b);
        // Kinds rendered
        assert!(section.html.contains("github-file"));
        assert!(section.html.contains("github-tree"));
        // Clickable <a href>
        assert!(section.html.contains("href=\"https://example.com/a\""));
    }

    #[test]
    fn rejected_sources_do_not_appear() {
        let f = write_jsonl(&[
            r#"{"event":"source_accepted","timestamp":"2026-04-19T10:01:00Z","url":"https://ok.test/","kind":"k","executor":"postagent","raw_path":"r","bytes":50,"trust_score":2.0}"#,
            r#"{"event":"source_rejected","timestamp":"2026-04-19T10:02:00Z","url":"https://bad.test/","kind":"k","executor":"postagent","reason":"duplicate"}"#,
        ]);
        let section = build_from_jsonl(f.path());
        assert_eq!(section.count, 1);
        assert!(section.html.contains("ok.test"));
        assert!(!section.html.contains("bad.test"));
    }

    #[test]
    fn sort_by_timestamp_ascending() {
        // Insert in reverse timestamp order — output should be re-sorted.
        let f = write_jsonl(&[
            r#"{"event":"source_accepted","timestamp":"2026-04-19T10:03:00Z","url":"https://third.test/","kind":"k","executor":"postagent","raw_path":"r","bytes":3,"trust_score":2.0}"#,
            r#"{"event":"source_accepted","timestamp":"2026-04-19T10:01:00Z","url":"https://first.test/","kind":"k","executor":"postagent","raw_path":"r","bytes":1,"trust_score":2.0}"#,
            r#"{"event":"source_accepted","timestamp":"2026-04-19T10:02:00Z","url":"https://second.test/","kind":"k","executor":"postagent","raw_path":"r","bytes":2,"trust_score":2.0}"#,
        ]);
        let section = build_from_jsonl(f.path());
        let pos1 = section.html.find("first.test").unwrap();
        let pos2 = section.html.find("second.test").unwrap();
        let pos3 = section.html.find("third.test").unwrap();
        assert!(pos1 < pos2 && pos2 < pos3);
    }

    #[test]
    fn html_escapes_malicious_url() {
        let f = write_jsonl(&[
            r#"{"event":"source_accepted","timestamp":"2026-04-19T10:01:00Z","url":"https://ex.test/?q=<script>","kind":"k","executor":"postagent","raw_path":"r","bytes":1,"trust_score":2.0}"#,
        ]);
        let section = build_from_jsonl(f.path());
        assert!(section.html.contains("&lt;script&gt;"));
        assert!(!section.html.contains("<script>"));
    }
}
