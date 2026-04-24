//! Embedded HTML template + placeholder substitution.
//!
//! The template is compiled into the binary via `include_str!` — agents do not
//! ship their own template in v1. Placeholder contract (see
//! `specs/research-report-templates.spec.md`):
//!
//! | Placeholder         | Filled from                                           |
//! |---------------------|-------------------------------------------------------|
//! | `{{TITLE}}`         | session topic                                         |
//! | `{{SUBTITLE}}`      | slug + tags + top-level source links                  |
//! | `{{ASIDE_QUOTE}}`   | first `> **aside:**` block (optional, may be empty)   |
//! | `{{BODY_HTML}}`     | markdown → HTML of sections                           |
//! | `{{SOURCES_HTML}}`  | `<ul>` built from session.jsonl accepted events       |
//! | `{{GENERATED_AT}}`  | RFC3339 UTC timestamp                                 |
//! | `{{SESSION_FOOTER}}`| absolute session path + counts                        |
//! | `{{LANG_SWITCH_HTML}}`| bilingual toggle; zh disabled unless translations exist |

pub const RICH_REPORT_HTML: &str = include_str!("../../templates/rich-report.html");

#[derive(Debug, Clone)]
pub struct Slots {
    pub title: String,
    pub subtitle: String,
    pub aside_quote: String,
    pub body_html: String,
    pub sources_html: String,
    pub generated_at: String,
    pub session_footer: String,
}

/// Substitute all placeholders. Each placeholder is replaced exactly once with
/// `replacen(… , 1)`-style semantics is unnecessary here because the template
/// ships with one occurrence of each marker (verified by the
/// `template_has_one_of_each_placeholder` test).
pub fn render(slots: &Slots) -> String {
    let lang_switch_html = if has_zh_translations(&slots.body_html) {
        r#"<div class="lang-switch" role="group" aria-label="language toggle">
    <button type="button" data-mode="en" class="active" onclick="document.body.classList.remove('bilingual'); this.classList.add('active'); this.nextElementSibling.classList.remove('active');">EN</button><button type="button" data-mode="zh" onclick="document.body.classList.add('bilingual'); this.classList.add('active'); this.previousElementSibling.classList.remove('active');">中文</button>
  </div>"#
    } else {
        r#"<div class="lang-switch" role="group" aria-label="language toggle">
    <button type="button" data-mode="en" class="active">EN</button><button type="button" data-mode="zh" disabled title="未生成中文翻译">中文</button>
  </div>"#
    };

    RICH_REPORT_HTML
        .replace("{{LANG_SWITCH_HTML}}", lang_switch_html)
        .replace("{{TITLE}}", &slots.title)
        .replace("{{SUBTITLE}}", &slots.subtitle)
        .replace("{{ASIDE_QUOTE}}", &slots.aside_quote)
        .replace("{{BODY_HTML}}", &slots.body_html)
        .replace("{{SOURCES_HTML}}", &slots.sources_html)
        .replace("{{GENERATED_AT}}", &slots.generated_at)
        .replace("{{SESSION_FOOTER}}", &slots.session_footer)
}

fn has_zh_translations(body_html: &str) -> bool {
    body_html.contains(r#"class="tr-zh""#) || body_html.contains(r#"class='tr-zh'"#)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_occurrences(s: &str, needle: &str) -> usize {
        s.matches(needle).count()
    }

    #[test]
    fn template_has_each_placeholder() {
        // TITLE appears twice (in <title> and <h1>) — allowed. Others must be present.
        assert!(RICH_REPORT_HTML.contains("{{TITLE}}"));
        assert_eq!(count_occurrences(RICH_REPORT_HTML, "{{SUBTITLE}}"), 1);
        assert_eq!(count_occurrences(RICH_REPORT_HTML, "{{ASIDE_QUOTE}}"), 1);
        assert_eq!(count_occurrences(RICH_REPORT_HTML, "{{BODY_HTML}}"), 1);
        assert_eq!(count_occurrences(RICH_REPORT_HTML, "{{SOURCES_HTML}}"), 1);
        assert_eq!(count_occurrences(RICH_REPORT_HTML, "{{GENERATED_AT}}"), 1);
        assert_eq!(count_occurrences(RICH_REPORT_HTML, "{{SESSION_FOOTER}}"), 1);
        assert_eq!(
            count_occurrences(RICH_REPORT_HTML, "{{LANG_SWITCH_HTML}}"),
            1
        );
    }

    #[test]
    fn render_disables_zh_language_switch_without_translations() {
        let slots = Slots {
            title: "Tokio".into(),
            subtitle: "sub".into(),
            aside_quote: String::new(),
            body_html: "<p>English only.</p>".into(),
            sources_html: "<ul></ul>".into(),
            generated_at: "2026-04-19T00:00:00Z".into(),
            session_footer: "/tmp/x".into(),
        };
        let out = render(&slots);
        assert!(out.contains(r#"<div class="lang-switch""#));
        assert!(out.contains(r#"disabled title="未生成中文翻译""#));
        assert!(out.contains("中文</button>"));
        assert!(!out.contains("{{LANG_SWITCH_HTML}}"));
    }

    #[test]
    fn render_includes_language_switch_when_translations_exist() {
        let slots = Slots {
            title: "Tokio".into(),
            subtitle: "sub".into(),
            aside_quote: String::new(),
            body_html: r#"<p>English.</p><p class="tr-zh" lang="zh-CN">中文。</p>"#.into(),
            sources_html: "<ul></ul>".into(),
            generated_at: "2026-04-19T00:00:00Z".into(),
            session_footer: "/tmp/x".into(),
        };
        let out = render(&slots);
        assert!(out.contains("lang-switch"));
        assert!(out.contains("中文</button>"));
        assert!(!out.contains("{{LANG_SWITCH_HTML}}"));
    }

    #[test]
    fn render_substitutes_all_slots() {
        let slots = Slots {
            title: "Tokio".into(),
            subtitle: "sub".into(),
            aside_quote: "<p class=\"aside\">q</p>".into(),
            body_html: "<h2>01</h2>".into(),
            sources_html: "<ul></ul>".into(),
            generated_at: "2026-04-19T00:00:00Z".into(),
            session_footer: "/tmp/x".into(),
        };
        let out = render(&slots);
        assert!(out.contains("<title>Tokio</title>"));
        assert!(out.contains("<h1>Tokio</h1>"));
        assert!(out.contains("sub"));
        assert!(out.contains("<p class=\"aside\">q</p>"));
        assert!(out.contains("<h2>01</h2>"));
        assert!(out.contains("<ul></ul>"));
        assert!(out.contains("2026-04-19T00:00:00Z"));
        assert!(!out.contains("{{"), "no placeholders should remain");
    }

    #[test]
    fn template_is_not_empty_and_has_doctype() {
        assert!(RICH_REPORT_HTML.len() > 1000);
        assert!(RICH_REPORT_HTML.starts_with("<!DOCTYPE html>"));
    }
}
