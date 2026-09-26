//! Exa API client: `/search` (fallback engine) and `/contents` (primary fetch).

use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use serde_json::Value;

use crate::degrade::SearchHit;
use crate::fetch::FetchDoc;
use crate::util::truncate_chars;

const BASE: &str = "https://api.exa.ai";

async fn post(client: &reqwest::Client, path: &str, key: &str, body: Value, timeout: Duration) -> Result<Value> {
    let response = client
        .post(format!("{BASE}{path}"))
        .header("x-api-key", key)
        .json(&body)
        .timeout(timeout)
        .send()
        .await
        .context("exa request failed")?;
    let status = response.status();
    let text = response.text().await.context("exa body read failed")?;
    if !status.is_success() {
        bail!("exa http {status}: {}", truncate_chars(&text, 200));
    }
    serde_json::from_str(&text).context("exa json parse failed")
}

pub(crate) async fn search(
    client: &reqwest::Client,
    key: &str,
    query: &str,
    num_results: u32,
    timeout: Duration,
) -> Result<Vec<SearchHit>> {
    let body = serde_json::json!({
        "query": query,
        "numResults": num_results,
        "type": "auto",
        "contents": {"highlights": {"numSentences": 2, "highlightsPerUrl": 1}},
    });
    let data = post(client, "/search", key, body, timeout).await?;
    let mut hits = Vec::new();
    let empty = Vec::new();
    let results = data
        .get("results")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    for item in results.iter().take(num_results as usize) {
        let url = item.get("url").and_then(Value::as_str).unwrap_or_default();
        if url.is_empty() {
            continue;
        }
        let snippet = item
            .get("highlights")
            .and_then(Value::as_array)
            .and_then(|h| h.first())
            .and_then(Value::as_str)
            .or_else(|| item.get("text").and_then(Value::as_str))
            .unwrap_or_default();
        let mut hit = SearchHit::new(
            item.get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            url.to_string(),
            truncate_chars(snippet, 400),
        );
        hit.published = item
            .get("publishedDate")
            .and_then(Value::as_str)
            .filter(|d| !d.is_empty())
            .map(str::to_string);
        hits.push(hit);
    }
    Ok(hits)
}

pub(crate) async fn fetch_contents(
    client: &reqwest::Client,
    key: &str,
    url: &str,
    max_chars: usize,
    timeout: Duration,
) -> Result<FetchDoc> {
    let body = serde_json::json!({
        "urls": [url],
        "text": {"maxCharacters": max_chars},
    });
    let data = post(client, "/contents", key, body, timeout).await?;
    let first = data
        .get("results")
        .and_then(Value::as_array)
        .and_then(|r| r.first());
    let text = first
        .and_then(|r| r.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let title = first
        .and_then(|r| r.get("title"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // Exa's failure mode is "200 but empty" — surface the provider error note.
    let note = if text.trim().is_empty() {
        Some(
            data.get("error")
                .and_then(Value::as_str)
                .unwrap_or("empty text")
                .to_string(),
        )
    } else {
        None
    };
    Ok(FetchDoc { title, text, note })
}
