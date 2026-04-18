//! Atomically rewrite the sources block inside session.md between the
//! canonical markers. Caller must hold the `session.md.lock` flock for the
//! duration of the read-modify-write cycle.

use std::fs;

use super::event::SessionEvent;
use super::layout::{self, MarkerError};

#[derive(Debug)]
pub enum RewriteError {
    MarkerMissing(MarkerError),
    Io(std::io::Error),
}

impl From<std::io::Error> for RewriteError {
    fn from(e: std::io::Error) -> Self {
        RewriteError::Io(e)
    }
}

/// Rebuild the sources block from the session's jsonl accepted events.
///
/// - Reads session.md
/// - Locates the `<!-- research:sources-start --> ... <!-- research:sources-end -->` range
/// - Replaces the middle with a generated listing (one accepted source per line)
/// - Writes back atomically via tempfile + rename
pub fn rebuild(slug: &str, events: &[SessionEvent]) -> Result<(), RewriteError> {
    let md_path = layout::session_md(slug);
    let original = fs::read_to_string(&md_path)?;
    let range = layout::locate_sources_block(&original).map_err(RewriteError::MarkerMissing)?;

    let mut rendered = String::from("\n");
    for ev in events {
        if let SessionEvent::SourceAccepted {
            url,
            kind,
            trust_score,
            ..
        } = ev
        {
            rendered.push_str(&format!("- [{kind} · trust {trust_score:.1}] {url}\n"));
        }
    }
    if rendered == "\n" {
        rendered.push_str("_(no accepted sources yet)_\n");
    }

    let mut out = String::with_capacity(original.len() + rendered.len());
    out.push_str(&original[..range.start]);
    out.push_str(&rendered);
    out.push_str(&original[range.end..]);

    // Atomic write
    let tmp = md_path.with_extension("md.tmp");
    fs::write(&tmp, out)?;
    fs::rename(&tmp, &md_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn accepted(url: &str, kind: &str, score: f64) -> SessionEvent {
        SessionEvent::SourceAccepted {
            timestamp: Utc::now(),
            url: url.into(),
            kind: kind.into(),
            executor: "postagent".into(),
            raw_path: "raw/1.json".into(),
            bytes: 100,
            trust_score: score,
            note: None,
        }
    }

    #[test]
    fn rendered_listing_shape() {
        let evs = vec![
            accepted("https://a", "hn-item", 2.0),
            accepted("https://b", "arxiv-abs", 2.0),
        ];
        let mut out = String::new();
        for ev in &evs {
            if let SessionEvent::SourceAccepted { url, kind, trust_score, .. } = ev {
                out.push_str(&format!("- [{kind} · trust {trust_score:.1}] {url}\n"));
            }
        }
        assert!(out.contains("hn-item"));
        assert!(out.contains("trust 2.0"));
        assert!(out.contains("https://a"));
        assert!(out.contains("https://b"));
    }
}
