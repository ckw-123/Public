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
        } else if rest.starts_with("&#39;") {
            ("&#39;", "'")
        } else if rest.starts_with("&apos;") {
            ("&apos;", "'")
        } else if rest.starts_with("&lt;") {
            ("&lt;", "<")
        } else if rest.starts_with("&gt;") {
            ("&gt;", ">")
        } else if rest.starts_with("&nbsp;") {
            ("&nbsp;", " ")
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
