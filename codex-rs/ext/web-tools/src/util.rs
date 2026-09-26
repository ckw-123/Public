//! Small shared helpers (HTML unescape/tag-stripping, truncation, turn items).

use std::sync::LazyLock;

use codex_extension_api::ExtensionTurnItem;
use codex_extension_items::ExtensionItem;
use codex_extension_items::web_search::WebSearchItem;
use codex_protocol::protocol::EventMsg;
use regex::Regex;

/// Compile a static regex. Patterns are compile-time constants in this crate,
/// so an invalid pattern is a bug, not a runtime condition.
pub(crate) fn static_regex(pattern: &str) -> Regex {
    match Regex::new(pattern) {
        Ok(re) => re,
        Err(err) => panic!("invalid static regex {pattern}: {err}"),
    }
}

static TAG: LazyLock<Regex> = LazyLock::new(|| static_regex(r"(?s)<[^>]+>"));
static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| static_regex(r"\s+"));

/// Decode a numeric HTML entity (`&#123;` / `&#x1F600;`) at the start of
/// `s`. Returns (consumed bytes, decoded char); None when malformed or not
/// a valid Unicode scalar (Bing emits zero-padded forms like `&#0183;`).
fn decode_numeric_entity(s: &str) -> Option<(usize, char)> {
    let body = s.strip_prefix("&#")?;
    let (num, radix) = match body.strip_prefix(['x', 'X']) {
        Some(hex) => (hex, 16),
        None => (body, 10),
    };
    let end = num.find(|c: char| !c.is_digit(radix))?;
    if end == 0 || num.as_bytes()[end] != b';' {
        return None;
    }
    let value = u32::from_str_radix(&num[..end], radix).ok()?;
    let ch = char::from_u32(value)?;
    Some((2 + (body.len() - num.len()) + end + 1, ch))
}

/// Minimal single-pass HTML entity unescape for SERP fragments.
pub(crate) fn unescape_html(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        let (entity, decoded) = if rest.starts_with("&amp;") {
            ("&amp;", "&")
        } else if rest.starts_with("&quot;") {
            ("&quot;", "\"")
        } else if rest.starts_with("&apos;") {
            ("&apos;", "'")
        } else if rest.starts_with("&lt;") {
            ("&lt;", "<")
        } else if rest.starts_with("&gt;") {
            ("&gt;", ">")
        } else if rest.starts_with("&nbsp;") {
            ("&nbsp;", " ")
        } else if let Some((len, ch)) = decode_numeric_entity(rest) {
            out.push(ch);
            rest = &rest[len..];
            continue;
        } else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        out.push_str(decoded);
        rest = &rest[entity.len()..];
    }
    out.push_str(rest);
    out
}

/// Strip tags, unescape entities, collapse whitespace (webtool.py `_text_of`).
pub(crate) fn text_of(fragment: &str) -> String {
    let no_tags = TAG.replace_all(fragment, "");
    let unescaped = unescape_html(&no_tags);
    WHITESPACE.replace_all(&unescaped, " ").trim().to_string()
}

/// Truncate to at most `max_chars` characters.
pub(crate) fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

/// Truncate to at most `max_bytes` bytes on a UTF-8 char boundary.
pub(crate) fn truncate_bytes_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Wrap an extension item with its legacy compatibility event (mirrors the
/// upstream web-search extension so existing TUI/status surfaces render it).
pub(crate) fn extension_turn_item(item: WebSearchItem, legacy_event: EventMsg) -> ExtensionTurnItem {
    ExtensionTurnItem {
        item: ExtensionItem::WebSearch(item),
        legacy_events: vec![legacy_event],
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::text_of;
    use super::truncate_bytes_boundary;
    use super::unescape_html;

    #[test]
    fn unescape_is_single_pass() {
        assert_eq!(unescape_html("&amp;quot;"), "&quot;");
        assert_eq!(unescape_html("a&amp;b&lt;c&gt;&quot;d&#39;e"), "a&b<c>\"d'e");
        assert_eq!(unescape_html("plain"), "plain");
        assert_eq!(unescape_html("100% & more"), "100% & more");
    }

    #[test]
    fn unescapes_numeric_entities() {
        // Bing snippets arrive zero-padded: `&#0183;` = `·`, `&#32;` = space.
        assert_eq!(unescape_html("Jan 23, 2026&#0183;&#32;x"), "Jan 23, 2026· x");
        assert_eq!(unescape_html("&#x4E2D;&#x6587;&#39;"), "中文'");
        // Malformed / invalid scalars stay literal.
        assert_eq!(unescape_html("&#;"), "&#;");
        assert_eq!(unescape_html("&#xD800;"), "&#xD800;");
        assert_eq!(unescape_html("&#99999999;"), "&#99999999;");
    }

    #[test]
    fn strips_tags_and_collapses_whitespace() {
        assert_eq!(text_of("<b>Hello</b>  <i>world</i>"), "Hello world");
        assert_eq!(text_of("&lt;tag&gt;"), "<tag>");
    }

    #[test]
    fn truncates_on_char_boundary() {
        assert_eq!(truncate_bytes_boundary("a中b", 2), "a");
        assert_eq!(truncate_bytes_boundary("a中b", 4), "a中");
        assert_eq!(truncate_bytes_boundary("abc", 10), "abc");
    }
}
