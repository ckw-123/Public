//! Relevance scoring and degradation detection for search results.
//!
//! Ported from the field-tested `webtool.py` logic; thresholds come from
//! `spike/bing_findings.md`. Degraded SERPs have a distinctive signature:
//! homepage-heavy, near-zero query-token overlap. Detection drives retry and
//! engine-fallback decisions; `cn.bing.com` in the final URL is only an
//! attribution label, never a degradation criterion by itself (direct +
//! `ensearch=1` also lands on cn.bing.com with perfectly good results).

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::util::static_regex;

/// Minimum query-token overlap ratio for a result to count as relevant.
const MIN_OVERLAP: f32 = 0.20;
/// Minimum matched query tokens: CJK queries tokenize into dense bigrams, so
/// require 2; latin-only multi-word queries legitimately match few tokens
/// (findings: `MIN_MATCHED_TOKENS=2` over-triggered fallback for them).
const MIN_MATCHED_TOKENS_CJK: usize = 2;
const MIN_MATCHED_TOKENS_LATIN: usize = 1;
/// Below this kept/results ratio the SERP counts as degraded.
const MIN_KEPT_RATIO: f32 = 0.40;
/// At or above this homepage-result ratio the SERP counts as degraded.
const HOMEPAGE_RATIO: f32 = 0.60;

const STOPWORDS: &[&str] = &[
    "的", "了", "和", "与", "及", "是", "在", "有", "为", "我", "你", "他", "它", "们", "这", "那",
    "就", "都", "也", "又", "而", "但", "或", "一个", "什么", "怎么", "如何", "the", "a", "an",
    "of", "to", "in", "on", "for", "and", "or", "is", "are", "with", "how", "what", "why",
    "when", "where",
];

static LATIN_TOKEN: LazyLock<Regex> = LazyLock::new(|| static_regex(r"[a-z0-9_\-]{2,}"));
static CJK_CHAR: LazyLock<Regex> = LazyLock::new(|| static_regex(r"[\u{4e00}-\u{9fff}]"));

/// One ranked search result.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SearchHit {
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) snippet: String,
    pub(crate) published: Option<String>,
}

impl SearchHit {
    pub(crate) fn new(title: String, url: String, snippet: String) -> Self {
        Self {
            title,
            url,
            snippet,
            published: None,
        }
    }
}

pub(crate) fn has_cjk(text: &str) -> bool {
    CJK_CHAR.is_match(text)
}

fn tokens(text: &str) -> HashSet<String> {
    let lower = text.to_lowercase();
    let mut out = HashSet::new();
    for m in LATIN_TOKEN.find_iter(&lower) {
        let word = m.as_str();
        if !STOPWORDS.contains(&word) {
            out.insert(word.to_string());
        }
    }
    let cjk: Vec<char> = lower
        .chars()
        .filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c))
        .collect();
    for pair in cjk.windows(2) {
        let gram: String = pair.iter().collect();
        if !STOPWORDS.contains(&gram.as_str()) {
            out.insert(gram);
        }
    }
    out
}

/// (overlap ratio, matched token count) of query tokens against `text`.
fn match_stats(query: &str, text: &str) -> (f32, usize) {
    let query_tokens = tokens(query);
    if query_tokens.is_empty() {
        return (0.0, 0);
    }
    let text_tokens = tokens(text);
    let hit = query_tokens.intersection(&text_tokens).count();
    (hit as f32 / query_tokens.len() as f32, hit)
}

fn is_homepage(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    matches!(
        parsed.path().trim_matches('/'),
        "" | "index.html" | "index.htm" | "index.php"
    )
}

/// Results split by the relevance filter plus aggregate degradation signals.
#[derive(Debug)]
pub(crate) struct ScoredResults {
    pub(crate) kept: Vec<SearchHit>,
    pub(crate) total: usize,
    pub(crate) homepages: usize,
}

pub(crate) fn score_results(results: Vec<SearchHit>, query: &str) -> ScoredResults {
    let min_matched = if has_cjk(query) {
        MIN_MATCHED_TOKENS_CJK
    } else {
        MIN_MATCHED_TOKENS_LATIN
    };
    let total = results.len();
    let homepages = results.iter().filter(|r| is_homepage(&r.url)).count();
    let mut kept = Vec::new();
    for hit in results {
        let (overlap, matched) = match_stats(query, &format!("{} {}", hit.title, hit.snippet));
        if overlap >= MIN_OVERLAP && matched >= min_matched {
            kept.push(hit);
        }
    }
    ScoredResults {
        kept,
        total,
        homepages,
    }
}

/// Degradation reasons; empty means a healthy SERP.
pub(crate) fn degradation_reasons(final_url: &str, scored: &ScoredResults) -> Vec<String> {
    let mut reasons = Vec::new();
    let cn_market = url::Url::parse(final_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_lowercase))
        .is_some_and(|host| host.ends_with("cn.bing.com"));
    if scored.total == 0 {
        reasons.push("no-results".to_string());
        if cn_market {
            reasons.push("cn-market-redirect".to_string());
        }
        return reasons;
    }
    let total = scored.total as f32;
    if (scored.kept.len() as f32) / total < MIN_KEPT_RATIO {
        reasons.push(format!("low-kept-ratio({}/{})", scored.kept.len(), scored.total));
    }
    if (scored.homepages as f32) / total >= HOMEPAGE_RATIO {
        reasons.push("homepage-heavy".to_string());
    }
    if cn_market && !reasons.is_empty() {
        reasons.push("cn-market-redirect".to_string());
    }
    reasons
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::SearchHit;
    use super::degradation_reasons;
    use super::has_cjk;
    use super::is_homepage;
    use super::score_results;

    fn hit(title: &str, url: &str, snippet: &str) -> SearchHit {
        SearchHit::new(title.to_string(), url.to_string(), snippet.to_string())
    }

    #[test]
    fn cjk_detection() {
        assert!(has_cjk("阿里云 百炼"));
        assert!(!has_cjk("codex responses api"));
    }

    #[test]
    fn homepage_detection() {
        assert!(is_homepage("https://www.aliyun.com/"));
        assert!(is_homepage("https://www.aliyun.com"));
        assert!(!is_homepage("https://help.aliyun.com/zh/model-studio/web-extractor"));
    }

    #[test]
    fn healthy_serp_has_no_reasons() {
        let results = vec![
            hit(
                "网页抓取-阿里云百炼",
                "https://help.aliyun.com/zh/model-studio/web-extractor",
                "阿里云百炼 web_extractor 网页抓取工具文档",
            ),
            hit(
                "web-extractor 计费说明",
                "https://docs.bailian.console.aliyun.com/web-extractor",
                "阿里云百炼 web_extractor 计费",
            ),
            hit(
                "alibabacloud 百炼控制台",
                "https://alibabacloud.com/help/bailian",
                "阿里云百炼 文档",
            ),
        ];
        let scored = score_results(results, "阿里云百炼 web_extractor 网页抓取工具");
        let reasons = degradation_reasons("https://cn.bing.com/search?q=x&ensearch=1", &scored);
        assert!(scored.kept.len() >= 2, "kept={:?}", scored.kept);
        assert!(reasons.is_empty(), "reasons={reasons:?}");
    }

    #[test]
    fn brand_navigation_serp_is_degraded() {
        let results = vec![
            hit("阿里云首页", "https://www.aliyun.com/", "阿里云官网"),
            hit("阿里集团", "https://www.alibabagroup.com/", "阿里巴巴集团"),
            hit("1688", "https://www.1688.com/", "批发网"),
            hit("校招", "https://talent.aliyun.com/", "校园招聘"),
        ];
        let scored = score_results(results, "阿里云百炼 web_extractor 网页抓取工具");
        let reasons = degradation_reasons("https://cn.bing.com/search?q=x", &scored);
        assert!(reasons.iter().any(|r| r.starts_with("low-kept-ratio")), "{reasons:?}");
        assert!(reasons.contains(&"homepage-heavy".to_string()), "{reasons:?}");
    }

    #[test]
    fn empty_serp_is_degraded() {
        let scored = score_results(Vec::new(), "anything");
        let reasons = degradation_reasons("https://cn.bing.com/search?q=x", &scored);
        assert_eq!(reasons, vec!["no-results", "cn-market-redirect"]);
    }

    #[test]
    fn cn_host_alone_is_not_degradation() {
        let results = vec![
            hit("codex responses-api-proxy", "https://github.com/openai/codex/blob/main/x.md", "codex responses api proxy docs"),
            hit("responses api", "https://github.com/openai/codex", "codex responses-api-proxy"),
        ];
        let scored = score_results(results, "codex responses-api-proxy");
        let reasons = degradation_reasons("https://cn.bing.com/search?q=x&ensearch=1", &scored);
        assert!(reasons.is_empty(), "{reasons:?}");
    }
}
