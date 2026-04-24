//! SVG safety validation for the v2 `write_diagram` action.
//!
//! The agent writes SVG source directly; the CLI trusts none of it until
//! it passes this validator. Enforced rules (from v2 spec):
//!
//! - size ≤ 512 KB
//! - must start with `<svg` (after optional leading whitespace + XML decl)
//! - must declare `xmlns="http://www.w3.org/2000/svg"` (single or double
//!   quotes accepted)
//! - must NOT contain, case-insensitive: `<script>`, `<foreignObject>`,
//!   any `on<name>=` attribute handler, or `javascript:` URLs
//!
//! The validator is deliberately a single pass of string checks on the
//! lowercased input — not a full XML parser. SVGs that pass here are
//! still sanitized by the rendering host at display time; this layer
//! exists to keep hostile primitives out of the repo in the first place.

pub const MAX_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvgRejection {
    Oversize { bytes: usize, max: usize },
    NotSvg,
    MissingXmlns,
    ContainsScript,
    ContainsForeignObject,
    ContainsOnHandler,
    ContainsJavascriptUrl,
}

impl std::fmt::Display for SvgRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SvgRejection::Oversize { bytes, max } => {
                write!(f, "svg_oversize: {bytes} bytes > {max} limit")
            }
            SvgRejection::NotSvg => {
                write!(f, "svg_not_svg: content does not start with `<svg`")
            }
            SvgRejection::MissingXmlns => write!(f, "svg_missing_xmlns"),
            SvgRejection::ContainsScript => write!(f, "svg_contains_script_tag"),
            SvgRejection::ContainsForeignObject => write!(f, "svg_contains_foreign_object"),
            SvgRejection::ContainsOnHandler => write!(f, "svg_contains_on_handler"),
            SvgRejection::ContainsJavascriptUrl => write!(f, "svg_contains_javascript_url"),
        }
    }
}

pub fn validate(svg: &str) -> Result<(), SvgRejection> {
    if svg.len() > MAX_BYTES {
        return Err(SvgRejection::Oversize {
            bytes: svg.len(),
            max: MAX_BYTES,
        });
    }
    let trimmed = strip_leading_xml_decl(svg);
    let lower = trimmed.to_lowercase();

    if !lower.starts_with("<svg") {
        return Err(SvgRejection::NotSvg);
    }
    if !trimmed.contains("xmlns=\"http://www.w3.org/2000/svg\"")
        && !trimmed.contains("xmlns='http://www.w3.org/2000/svg'")
    {
        return Err(SvgRejection::MissingXmlns);
    }
    if lower.contains("<script") {
        return Err(SvgRejection::ContainsScript);
    }
    if lower.contains("<foreignobject") {
        return Err(SvgRejection::ContainsForeignObject);
    }
    if contains_on_handler(&lower) {
        return Err(SvgRejection::ContainsOnHandler);
    }
    if lower.contains("javascript:") {
        return Err(SvgRejection::ContainsJavascriptUrl);
    }
    Ok(())
}

fn strip_leading_xml_decl(s: &str) -> &str {
    let s = s.trim_start();
    if let Some(rest) = s.strip_prefix("<?xml")
        && let Some(end) = rest.find("?>")
    {
        return rest[end + 2..].trim_start();
    }
    s
}

/// True if `lower` contains an `on<name>=` attribute pattern (preceded by
/// a non-alphanumeric boundary, optional whitespace between name and `=`).
fn contains_on_handler(lower: &str) -> bool {
    let bytes = lower.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        let prev = if i == 0 { b' ' } else { bytes[i - 1] };
        if !prev.is_ascii_alphanumeric()
            && bytes[i] == b'o'
            && bytes[i + 1] == b'n'
            && bytes[i + 2].is_ascii_alphabetic()
        {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                j += 1;
            }
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'=' {
                return true;
            }
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simple_quadrant() -> &'static str {
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 400 400">
            <rect width="400" height="400" fill="white"/>
            <line x1="0" y1="200" x2="400" y2="200" stroke="black"/>
            <line x1="200" y1="0" x2="200" y2="400" stroke="black"/>
        </svg>"#
    }

    #[test]
    fn svg_safety_accepts_simple_quadrant() {
        assert!(validate(simple_quadrant()).is_ok());
    }

    #[test]
    fn accepts_xml_decl_and_whitespace_prefix() {
        let svg =
            "  \n<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>";
        assert!(validate(svg).is_ok());
    }

    #[test]
    fn accepts_single_quoted_xmlns() {
        let svg = "<svg xmlns='http://www.w3.org/2000/svg'><rect/></svg>";
        assert!(validate(svg).is_ok());
    }

    #[test]
    fn svg_safety_rejects_not_svg() {
        assert_eq!(
            validate("<html><body>not svg</body></html>").unwrap_err(),
            SvgRejection::NotSvg
        );
    }

    #[test]
    fn svg_safety_rejects_missing_xmlns() {
        let svg = r#"<svg viewBox="0 0 100 100"><rect/></svg>"#;
        assert_eq!(validate(svg).unwrap_err(), SvgRejection::MissingXmlns);
    }

    #[test]
    fn svg_safety_rejects_script_tag() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
        assert_eq!(validate(svg).unwrap_err(), SvgRejection::ContainsScript);
    }

    #[test]
    fn rejects_script_tag_case_insensitive() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><ScRipT>x</ScRipT></svg>"#;
        assert_eq!(validate(svg).unwrap_err(), SvgRejection::ContainsScript);
    }

    #[test]
    fn rejects_foreign_object() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><foreignObject><div/></foreignObject></svg>"#;
        assert_eq!(
            validate(svg).unwrap_err(),
            SvgRejection::ContainsForeignObject
        );
    }

    #[test]
    fn svg_safety_rejects_on_handler_attr() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><rect onclick="alert(1)"/></svg>"#;
        assert_eq!(validate(svg).unwrap_err(), SvgRejection::ContainsOnHandler);
    }

    #[test]
    fn rejects_on_handler_with_space_before_equals() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><rect onload ="x"/></svg>"#;
        assert_eq!(validate(svg).unwrap_err(), SvgRejection::ContainsOnHandler);
    }

    #[test]
    fn accepts_polygon_not_an_on_handler() {
        // "polygon" contains the substring "on" but is not a handler.
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><polygon points="0,0 1,1"/></svg>"#;
        assert!(
            validate(svg).is_ok(),
            "polygon must not trip on-handler check"
        );
    }

    #[test]
    fn rejects_javascript_url() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><a xlink:href="javascript:alert(1)"/></svg>"#;
        assert_eq!(
            validate(svg).unwrap_err(),
            SvgRejection::ContainsJavascriptUrl
        );
    }

    #[test]
    fn svg_safety_rejects_oversize() {
        let mut big = String::from(r#"<svg xmlns="http://www.w3.org/2000/svg">"#);
        big.push_str(&"x".repeat(MAX_BYTES));
        big.push_str("</svg>");
        assert!(matches!(
            validate(&big).unwrap_err(),
            SvgRejection::Oversize { .. }
        ));
    }
}
