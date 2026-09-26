//! Firecrawl API client: `/scrape` (fallback fetch; anti-bot pages and PDFs).

use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use serde_json::Value;

use crate::fetch::FetchDoc;
use crate::util::truncate_chars;

const BASE: &str = "https://api.firecrawl.dev";

pub(crate) async fn scrape(
    client: &reqwest::Client,
    key: &str,
    url: &str,
    timeout: Duration,
) -> Result<FetchDoc> {
    let body = serde_json::json!({"url": url, "formats": ["markdown"], "onlyMainContent": true});
    // v2 first, fall back to v1 on 404/405 only (webtool.py ladder).
    let mut status = 0u16;
    let mut text = String::new();
    for path in ["/v2/scrape", "/v1/scrape"] {
        let response = client
            .post(format!("{BASE}{path}"))
            .bearer_auth(key)
            .json(&body)
            .timeout(timeout)
            .send()
            .await
            .context("firecrawl request failed")?;
        status = response.status().as_u16();
        text = response.text().await.context("firecrawl body read failed")?;
        if (200..300).contains(&status) || (status != 404 && status != 405) {
            break;
        }
    }
    if !(200..300).contains(&status) {
        bail!("firecrawl http {status}: {}", truncate_chars(&text, 200));
    }
    let data: Value = serde_json::from_str(&text).context("firecrawl json parse failed")?;
    let doc = data.get("data");
    let markdown = doc
        .and_then(|d| d.get("markdown"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let title = doc
        .and_then(|d| d.get("metadata"))
        .and_then(|m| m.get("title"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let note = if markdown.trim().is_empty() {
        Some(
            data.get("error")
                .and_then(Value::as_str)
                .unwrap_or("empty markdown")
                .to_string(),
        )
    } else {
        None
    };
    Ok(FetchDoc {
        title,
        text: markdown,
        note,
    })
}
