//! Bing web-search HTML client: stateless `cn.bing.com` + `ensearch=1`.
//!
//! Per `spike/bing_findings.md`:
//! - requests are stateless (no cookies, no priming); `ensearch=1` on the
//!   search URL is the only reliable switch into the international index;
//! - degradation is a per-request transient event: detected from the result
//!   signature, recovered by an immediate same-URL retry;
//! - the optional proxy rung lands on `www.bing.com` (international exit).

use std::sync::LazyLock;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE;
use regex::Regex;

use crate::degrade::SearchHit;
use crate::util::static_regex;
use crate::util::text_of;
use crate::util::unescape_html;

/// Direct rung: cn.bing.com pinned to the international index via ensearch=1.
const ENDPOINT_CN: &str = "https://cn.bing.com/search";
/// Proxy rung: with a non-CN exit the international SERP is served directly.
const ENDPOINT_INTL: &str = "https://www.bing.com/search";

pub(crate) const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

static B_ALGO_SPLIT: LazyLock<Regex> = LazyLock::new(|| static_regex(r#"(?is)<li class="b_algo""#));
static H2_LINK: LazyLock<Regex> =
    LazyLock::new(|| static_regex(r#"(?is)<h2[^>]*>\s*<a[^>]*href="([^"]+)""#));
static ANY_LINK: LazyLock<Regex> =
    LazyLock::new(|| static_regex(r#"(?is)<a[^>]*href="([^"]+)"[^>]*class="tilk""#));
static H2_ANY: LazyLock<Regex> = LazyLock::new(|| static_regex(r"(?is)<h2[^>]*>(.*?)</h2>"));
static SNIPPET: LazyLock<Regex> = LazyLock::new(|| static_regex(r"(?is)<p[^>]*>(.*?)</p>"));
static CK_U: LazyLock<Regex> = LazyLock::new(|| static_regex(r"[?&]u=a1([^&]+)"));

const CHALLENGE_MARKERS: &[&str] = &[
    "anomaly-modal",
    "anomaly.js",
    "g-recaptcha",
    "are you a robot",
    "unusual traffic",
    "verify you are human",
    "challenge-platform",
    "cf-challenge",
    "captcha",
];

/// One fetched and parsed SERP.
pub(crate) struct BingPage {
    pub(crate) status: u16,
    pub(crate) final_url: String,
    pub(crate) hits: Vec<SearchHit>,
    pub(crate) challenge: Option<&'static str>,
}

/// Fetch and parse one Bing SERP. `intl_host` selects the www.bing.com
/// endpoint (used for the proxy rung).
pub(crate) async fn search_page(
    client: &reqwest::Client,
    query: &str,
    count: u32,
    timeout: Duration,
    intl_host: bool,
) -> Result<BingPage> {
    let endpoint = if intl_host {
        ENDPOINT_INTL
    } else {
        ENDPOINT_CN
    };
    let mut url = url::Url::parse(endpoint).context("static bing endpoint should parse")?;
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("ensearch", "1")
        .append_pair("count", &count.to_string());
    let response = client
        .get(url)
        .timeout(timeout)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .header("Upgrade-Insecure-Requests", "1")
        .send()
        .await
        .context("bing request failed")?;
    let status = response.status().as_u16();
    let final_url = response.url().to_string();
    let text = response.text().await.context("bing body read failed")?;
    let challenge = detect_challenge(&text);
    let hits = parse_results(&text, count as usize);
    Ok(BingPage {
        status,
        final_url,
        hits,
        challenge,
    })
}

pub(crate) fn detect_challenge(html: &str) -> Option<&'static str> {
    let low = html.to_lowercase();
    CHALLENGE_MARKERS
        .iter()
        .copied()
        .find(|marker| low.contains(marker))
}

/// Unwrap `https://www.bing.com/ck/a?...&u=a1<base64url>` redirect URLs.
fn unwrap_bing_redirect(href: &str) -> String {
    let Some(cap) = CK_U.captures(href) else {
        return href.to_string();
    };
    let Some(m) = cap.get(1) else {
        return href.to_string();
    };
    let raw = m.as_str();
    let pad = (4 - raw.len() % 4) % 4;
    let padded = format!("{raw}{}", "=".repeat(pad));
    match URL_SAFE.decode(padded.as_bytes()) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => href.to_string(),
    }
}

/// Robust across Bing layouts: split on the `b_algo` marker (not `</li>`),
/// take the first h2/a per block, unescape entities then unwrap redirects.
pub(crate) fn parse_results(html: &str, limit: usize) -> Vec<SearchHit> {
    let mut out = Vec::new();
    for block in B_ALGO_SPLIT.split(html).skip(1) {
        let link = H2_LINK
            .captures(block)
            .or_else(|| ANY_LINK.captures(block));
        let Some(cap) = link else {
            continue;
        };
        let Some(href) = cap.get(1).map(|m| m.as_str()) else {
            continue;
        };
        let url = unwrap_bing_redirect(&unescape_html(href));
        if !url.starts_with("http") || url.contains("bing.com") {
            continue;
        }
        let title = H2_ANY
            .captures(block)
            .and_then(|c| c.get(1))
            .map(|m| text_of(m.as_str()))
            .unwrap_or_default();
        let snippet = SNIPPET
            .captures(block)
            .and_then(|c| c.get(1))
            .map(|m| text_of(m.as_str()))
            .unwrap_or_default();
        out.push(SearchHit::new(title, url, snippet));
        if out.len() >= limit {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::detect_challenge;
    use super::parse_results;
    use super::unwrap_bing_redirect;

    const SAMPLE_SERP: &str = r#"
    <html><body><ol id="b_results">
    <li class="b_algo"><h2><a href="https://help.aliyun.com/zh/model-studio/web-extractor">网页抓取-阿里云百炼</a></h2>
    <p>阿里云百炼 web_extractor 网页抓取工具文档</p></li>
    <li class="b_algo"><h2><a href="https://www.bing.com/ck/a?!&&p=abc&amp;u=a1aHR0cHM6Ly9leGFtcGxlLmNvbS9kb2Nz&amp;ntb=1">wrapped</a></h2>
    <p>snippet &lt;b&gt;two&lt;/b&gt;</p></li>
    <li class="b_ad"><h2><a href="https://ads.example.com/">ad block ignored</a></h2></li>
    </ol></body></html>
    "#;

    #[test]
    fn parses_b_algo_blocks() {
        let hits = parse_results(SAMPLE_SERP, 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "网页抓取-阿里云百炼");
        assert_eq!(
            hits[0].url,
            "https://help.aliyun.com/zh/model-studio/web-extractor"
        );
        assert!(hits[0].snippet.contains("web_extractor"));
        assert_eq!(hits[1].url, "https://example.com/docs");
        assert_eq!(hits[1].snippet, "snippet <b>two</b>");
    }

    #[test]
    fn respects_limit() {
        assert_eq!(parse_results(SAMPLE_SERP, 1).len(), 1);
    }

    #[test]
    fn unwraps_ck_redirect() {
        assert_eq!(
            unwrap_bing_redirect("https://www.bing.com/ck/a?!&&p=x&u=a1aHR0cHM6Ly9leGFtcGxlLmNvbS9kb2Nz&ntb=1"),
            "https://example.com/docs"
        );
        assert_eq!(unwrap_bing_redirect("https://example.com/a?u=xx"), "https://example.com/a?u=xx");
    }

    #[test]
    fn detects_challenge_pages() {
        assert_eq!(detect_challenge("<html>g-recaptcha</html>"), Some("g-recaptcha"));
        assert_eq!(detect_challenge("<html>normal page</html>"), None);
    }
}
