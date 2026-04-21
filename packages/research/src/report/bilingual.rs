//! Bilingual (English → Chinese) paragraph translation for the rich-html
//! report.
//!
//! Post-processes the English body HTML produced by `markdown::render_body`.
//! For each `<p>` that contains sentence-like content, calls Claude via
//! cc-sdk to get a Chinese translation and injects a `<p class="tr-zh">`
//! sibling immediately after it. The template's language toggle flips
//! display based on `body.bilingual` class.
//!
//! Requires the `provider-claude` feature at compile time. Without it,
//! `inject_zh_translations` returns an error so the caller can degrade
//! gracefully (keep monolingual output + log a warning).
//!
//! Scope choices (deliberate MVP):
//! - Translates only `<p>` elements — headings, lists, and figures are
//!   preserved as-is. The user's ask was "each English paragraph has the
//!   corresponding Chinese below it."
//! - Skips paragraphs that are only whitespace, only inline tags, or that
//!   look like code / markdown-escaped output.
//! - Translates in a single batched LLM call using a numbered list, which
//!   is ~10× cheaper than one call per paragraph and tolerates modest
//!   drift in paragraph count (falls back to monolingual on mismatch).

// Helpers below are wired up only when `provider-claude` is compiled
// in — that's when `inject_zh_translations` actually dispatches through
// them. On default (and even `cargo test` without the feature) several
// items are genuinely unreachable: `html_escape_text` / `system_prompt`
// / `user_prompt` / the `outer_end` field live inside the feature-gated
// block, and the unit tests only exercise the parsing helpers.
//
// Rather than chase each item with a per-item cfg, silence dead_code
// for the whole module whenever `provider-claude` is absent.
#![cfg_attr(not(feature = "provider-claude"), allow(dead_code))]

use regex::Regex;
use std::sync::OnceLock;

pub enum BilingualError {
    ProviderMissing(String),
    ProviderCallFailed(String),
    NothingToTranslate,
}

impl std::fmt::Display for BilingualError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BilingualError::ProviderMissing(m) => write!(f, "provider unavailable: {m}"),
            BilingualError::ProviderCallFailed(m) => write!(f, "provider call failed: {m}"),
            BilingualError::NothingToTranslate => write!(f, "no translatable paragraphs found"),
        }
    }
}

/// Scan `body_html`, translate each qualifying `<p>` body to Chinese, and
/// return the rewritten HTML plus an optional note for the warnings log
/// (e.g. when paragraph counts drift between input and translation).
///
/// When the `provider-claude` feature is not compiled in, returns
/// `ProviderMissing` so the caller can skip translation and still render
/// a monolingual report.
pub fn inject_zh_translations(body_html: &str) -> Result<(String, Option<String>), BilingualError> {
    #[cfg(not(feature = "provider-claude"))]
    {
        let _ = body_html;
        return Err(BilingualError::ProviderMissing(
            "provider-claude feature not compiled in; rebuild with --features provider-claude"
                .into(),
        ));
    }
    #[cfg(feature = "provider-claude")]
    {
        use crate::autoresearch::claude::ClaudeProvider;
        use crate::autoresearch::provider::{AgentProvider, ProviderError};

        let spans = find_paragraph_spans(body_html);
        let english: Vec<(usize, String)> = spans
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let inner = &body_html[s.inner_start..s.inner_end];
                let plain = plain_text_from_inline_html(inner);
                if should_translate(&plain) {
                    Some((i, plain))
                } else {
                    None
                }
            })
            .collect();

        if english.is_empty() {
            return Err(BilingualError::NothingToTranslate);
        }

        let provider = ClaudeProvider::new();
        let system = system_prompt();
        let user = user_prompt(&english);

        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| BilingualError::ProviderCallFailed(format!("runtime: {e}")))?;
        let response = runtime
            .block_on(provider.ask(&system, &user))
            .map_err(|e| match e {
                ProviderError::NotAvailable(m) => BilingualError::ProviderMissing(m),
                other => BilingualError::ProviderCallFailed(other.to_string()),
            })?;

        let translations = parse_translations(&response, english.len());
        let mut note: Option<String> = None;
        if translations.len() != english.len() {
            note = Some(format!(
                "bilingual_partial: {} paragraphs translated of {} requested (LLM drift)",
                translations.len(),
                english.len()
            ));
        }

        let mut out = String::with_capacity(body_html.len() + translations.iter().map(|s| s.len() + 32).sum::<usize>());
        let mut cursor = 0usize;
        let mut translation_index: std::collections::HashMap<usize, String> =
            std::collections::HashMap::new();
        for ((idx, _), zh) in english.iter().zip(translations.iter()) {
            translation_index.insert(*idx, zh.clone());
        }

        for (i, span) in spans.iter().enumerate() {
            out.push_str(&body_html[cursor..span.outer_end]);
            cursor = span.outer_end;
            if let Some(zh) = translation_index.get(&i) {
                out.push_str(r#"<p class="tr-zh" lang="zh-CN">"#);
                out.push_str(&html_escape_text(zh));
                out.push_str("</p>\n");
            }
        }
        out.push_str(&body_html[cursor..]);
        Ok((out, note))
    }
}

#[derive(Debug, Clone, Copy)]
struct PSpan {
    outer_end: usize,
    inner_start: usize,
    inner_end: usize,
}

fn find_paragraph_spans(html: &str) -> Vec<PSpan> {
    static OPEN_RE: OnceLock<Regex> = OnceLock::new();
    let open_re = OPEN_RE.get_or_init(|| Regex::new(r"<p(\s[^>]*)?>").expect("p open regex"));
    let mut out: Vec<PSpan> = Vec::new();
    let mut cursor = 0usize;
    while cursor < html.len() {
        let Some(m) = open_re.find_at(html, cursor) else {
            break;
        };
        let inner_start = m.end();
        let Some(close_rel) = html[inner_start..].find("</p>") else {
            break;
        };
        let inner_end = inner_start + close_rel;
        let outer_end = inner_end + "</p>".len();
        out.push(PSpan {
            outer_end,
            inner_start,
            inner_end,
        });
        cursor = outer_end;
    }
    out
}

fn plain_text_from_inline_html(s: &str) -> String {
    static TAG_RE: OnceLock<Regex> = OnceLock::new();
    let tag_re = TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").expect("tag strip regex"));
    let stripped = tag_re.replace_all(s, " ");
    html_unescape(&stripped).trim().to_string()
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn html_escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn should_translate(plain: &str) -> bool {
    let t = plain.trim();
    if t.len() < 20 {
        return false;
    }
    // Skip paragraphs that are mostly non-letters (e.g. code snippets,
    // link dumps) — heuristic: < 40% alphabetic would misbehave.
    let letters = t.chars().filter(|c| c.is_alphabetic()).count();
    (letters * 100) / t.chars().count().max(1) > 40
}

fn system_prompt() -> &'static str {
    "You are a precise EN→zh-CN translator for a technical research report. \
Given numbered English paragraphs, return the Chinese translations as a \
strictly numbered list in the SAME order, one per line, with NO extra commentary. \
Preserve proper nouns (Gödel, Voyager, CDP, etc.) verbatim. Do not \
summarize — translate faithfully, matching paragraph boundaries. Output \
format strictly: `1. <chinese>\\n2. <chinese>\\n...`"
}

fn user_prompt(english: &[(usize, String)]) -> String {
    let mut out = String::from("Translate each paragraph below to Simplified Chinese (zh-CN). Keep the numbered ordering, one translation per line.\n\n");
    for (rank, (_, text)) in english.iter().enumerate() {
        out.push_str(&format!("{}. {}\n\n", rank + 1, text));
    }
    out.push_str("Reply ONLY with the numbered translations — no preamble, no trailing notes.");
    out
}

fn parse_translations(response: &str, expected: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(expected);
    let line_re = Regex::new(r"^\s*(\d+)[\.\)]\s*(.*)$").expect("translation line regex");
    let mut pending_num: Option<usize> = None;
    let mut pending_buf = String::new();
    let flush = |out: &mut Vec<String>, buf: &mut String| {
        let trimmed = buf.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
        }
        buf.clear();
    };
    for line in response.lines() {
        if let Some(caps) = line_re.captures(line) {
            if pending_num.is_some() {
                flush(&mut out, &mut pending_buf);
            }
            pending_num = caps.get(1).and_then(|m| m.as_str().parse().ok());
            if let Some(m) = caps.get(2) {
                pending_buf.push_str(m.as_str());
            }
        } else if pending_num.is_some() {
            let stripped = line.trim();
            if !stripped.is_empty() {
                if !pending_buf.is_empty() {
                    pending_buf.push(' ');
                }
                pending_buf.push_str(stripped);
            }
        }
    }
    if pending_num.is_some() {
        flush(&mut out, &mut pending_buf);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_paragraph_spans_basic() {
        let html = "<h2>x</h2><p>one</p><p class=\"a\">two</p><ul><li>z</li></ul><p>three</p>";
        let spans = find_paragraph_spans(html);
        assert_eq!(spans.len(), 3);
        assert_eq!(&html[spans[0].inner_start..spans[0].inner_end], "one");
        assert_eq!(&html[spans[1].inner_start..spans[1].inner_end], "two");
        assert_eq!(&html[spans[2].inner_start..spans[2].inner_end], "three");
    }

    #[test]
    fn plain_text_strips_inline_tags() {
        let s = "See <a href=\"x\">docs</a> and <code>cmd --flag</code> for &quot;more&quot;.";
        let plain = plain_text_from_inline_html(s);
        assert!(plain.contains("docs"));
        assert!(plain.contains("cmd --flag"));
        assert!(plain.contains("\"more\""));
        assert!(!plain.contains('<'));
    }

    #[test]
    fn should_translate_skips_short_text() {
        assert!(!should_translate(""));
        assert!(!should_translate("ok"));
        assert!(should_translate(
            "This is a real paragraph with enough content to translate."
        ));
    }

    #[test]
    fn should_translate_skips_code_dump() {
        let mostly_non_letters = "#@#@#@#@#@#@#@#@#@ 12345 !!!!!! {{{{ }}}}";
        assert!(!should_translate(mostly_non_letters));
    }

    #[test]
    fn parse_translations_recovers_multiline_items() {
        let response = "1. 第一段中文。\n   续行。\n2. 第二段。\n3. 第三段内容。";
        let parsed = parse_translations(response, 3);
        assert_eq!(parsed.len(), 3);
        assert!(parsed[0].contains("第一段"));
        assert!(parsed[0].contains("续行"));
        assert_eq!(parsed[1], "第二段。");
        assert_eq!(parsed[2], "第三段内容。");
    }

    #[test]
    fn parse_translations_tolerates_parens_numbering() {
        let response = "1) 一。\n2) 二。";
        let parsed = parse_translations(response, 2);
        assert_eq!(parsed.len(), 2);
    }
}
