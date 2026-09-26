//! `search_web` tool: Bing direct + same-URL retries → optional proxy rung →
//! Exa last resort. See spike/bing_findings.md for the measured architecture.

use std::time::Duration;

use codex_extension_api::FunctionCallError;
use codex_extension_api::ResponsesApiTool;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolExecutor;
use codex_extension_api::ToolExecutorFuture;
use codex_extension_api::ToolName;
use codex_extension_api::ToolOutput;
use codex_extension_api::ToolSpec;
use codex_extension_api::parse_tool_input_schema;
use codex_extension_items::web_search::WebSearchAction;
use codex_extension_items::web_search::WebSearchItem;
use codex_protocol::models::WebSearchAction as CoreWebSearchAction;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::WebSearchBeginEvent;
use codex_protocol::protocol::WebSearchEndEvent;
use serde::Deserialize;

use crate::bing;
use crate::config::WebToolsConfig;
use crate::degrade::SearchHit;
use crate::degrade::degradation_reasons;
use crate::degrade::score_results;
use crate::exa;
use crate::output::WebToolOutput;
use crate::util::extension_turn_item;
use crate::util::truncate_chars;

pub(crate) const TOOL_NAME: &str = "search_web";
/// Degraded SERPs recover on immediate same-URL retry (per-request transient,
/// not sticky): 1 initial try + 2 retries.
const BING_MAX_ATTEMPTS: usize = 3;
const RETRY_DELAY: Duration = Duration::from_millis(500);

const DESCRIPTION: &str = "\
Search the public web and return ranked results (title, URL, snippet).

Routing is automatic: Bing (direct, international index) is tried first with
signature-based quality detection and automatic retries; an optional proxy
exit and then Exa serve as fallbacks when configured. The first output line
reports which engine answered and the quality status.

If the output says FAILED, every engine failed or was degraded: do not retry
the same query. Rephrase once, or answer from your own knowledge and note
that online verification failed.

Use this tool whenever the answer may depend on recent events, changing
documentation/APIs, prices, or facts you are unsure about.";

pub(crate) struct SearchWebTool {
    pub(crate) config: WebToolsConfig,
    pub(crate) client: reqwest::Client,
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    num_results: Option<u32>,
}

enum Rung {
    Good(Vec<SearchHit>, String),
    Bad(String),
}

impl<'call> ToolExecutor<ToolCall<'call>> for SearchWebTool {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        let parameters = match parse_tool_input_schema(&serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Search query. Keep it specific; 3-10 words work best."},
                "num_results": {"type": "integer", "minimum": 1, "maximum": 15,
                    "description": "Max results to return. Default 10."},
            },
            "required": ["query"],
            "additionalProperties": false,
        })) {
            Ok(parameters) => parameters,
            Err(err) => panic!("search_web schema should parse: {err}"),
        };
        ToolSpec::Function(ResponsesApiTool {
            name: TOOL_NAME.to_string(),
            description: DESCRIPTION.to_string(),
            strict: false,
            parameters,
            output_schema: None,
            defer_loading: None,
        })
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    fn handle<'a>(&'a self, call: ToolCall<'call>) -> ToolExecutorFuture<'a>
    where
        'call: 'a,
    {
        Box::pin(self.handle_call(call))
    }
}

impl SearchWebTool {
    /// One Bing attempt: fetch, relevance-score, classify degraded vs healthy.
    async fn bing_once(
        &self,
        client: &reqwest::Client,
        query: &str,
        count: u32,
        intl_host: bool,
        label: &str,
    ) -> Rung {
        let page = match bing::search_page(client, query, count, self.config.bing_timeout, intl_host)
            .await
        {
            Ok(page) => page,
            Err(err) => return Rung::Bad(format!("bing {label}: {err:#}")),
        };
        if page.status != 200 {
            return Rung::Bad(format!("bing {label}: http {}", page.status));
        }
        let scored = score_results(page.hits, query);
        let mut reasons = degradation_reasons(&page.final_url, &scored);
        if let Some(marker) = page.challenge {
            reasons.push(format!("challenge({marker})"));
        }
        if reasons.is_empty() {
            let note = format!("bing {label}, kept {}/{}", scored.kept.len(), scored.total);
            Rung::Good(scored.kept, note)
        } else {
            Rung::Bad(format!("bing {label}: degraded [{}]", reasons.join(",")))
        }
    }

    async fn handle_call(
        &self,
        call: ToolCall<'_>,
    ) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let args: SearchArgs = serde_json::from_str(call.function_arguments()?)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        let query = args.query.trim().to_string();
        if query.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "missing or empty `query`".to_string(),
            ));
        }
        let count = args
            .num_results
            .unwrap_or(self.config.search_max_results)
            .clamp(1, 15);

        call.turn_item_emitter
            .emit_started(extension_turn_item(
                WebSearchItem {
                    id: call.call_id.clone(),
                    query: String::new(),
                    action: None,
                    results: None,
                },
                EventMsg::WebSearchBegin(WebSearchBeginEvent {
                    call_id: call.call_id.clone(),
                }),
            ))
            .await;

        let mut attempts: Vec<String> = Vec::new();
        let mut outcome: Option<(Vec<SearchHit>, String)> = None;

        // Rung 1: Bing direct; degraded SERPs get an immediate same-URL retry.
        for attempt in 1..=BING_MAX_ATTEMPTS {
            if attempt > 1 {
                tokio::time::sleep(RETRY_DELAY).await;
            }
            match self
                .bing_once(&self.client, &query, count, false, "direct+ensearch")
                .await
            {
                Rung::Good(hits, note) => {
                    outcome = Some((hits, format!("{note} (attempt {attempt})")));
                    break;
                }
                Rung::Bad(note) => attempts.push(format!("#{attempt} {note}")),
            }
        }

        // Rung 2 (optional): same query via configured proxy exit (cross-route
        // second opinion; lands on www.bing.com).
        if outcome.is_none()
            && let Some(proxy_url) = &self.config.bing_proxy
        {
            let proxied = reqwest::Proxy::all(proxy_url.as_str()).and_then(|proxy| {
                reqwest::Client::builder()
                    .user_agent(bing::USER_AGENT)
                    .proxy(proxy)
                    .build()
            });
            match proxied {
                Ok(client) => match self.bing_once(&client, &query, count, true, "proxy+www").await {
                    Rung::Good(hits, note) => outcome = Some((hits, note)),
                    Rung::Bad(note) => attempts.push(note),
                },
                Err(err) => attempts.push(format!("bing proxy: invalid proxy config: {err:#}")),
            }
        }

        // Rung 3 (optional): Exa as last resort. Empty here + degraded Bing =
        // the query likely has no results ("route invariance" arbitration).
        let mut likely_no_results = false;
        if outcome.is_none()
            && let Some(key) = &self.config.exa_api_key
        {
            match exa::search(&self.client, key, &query, count, self.config.exa_timeout).await {
                Ok(hits) if !hits.is_empty() => {
                    outcome = Some((
                        hits,
                        format!("exa fallback (bing exhausted {} attempts)", attempts.len()),
                    ));
                }
                Ok(_) => {
                    likely_no_results = true;
                    attempts.push("exa: 0 results".to_string());
                }
                Err(err) => attempts.push(format!("exa: {err:#}")),
            }
        }

        let (text, success, header) = match outcome {
            Some((hits, header)) => (render_results(&hits, &header), true, header),
            None if likely_no_results => (
                format!(
                    "[search_web: no trustworthy results] \"{query}\" looks genuinely result-less \
                     (bing stayed degraded, exa returned 0). Tell the user the engines found \
                     nothing rather than retrying."
                ),
                true,
                "no-results".to_string(),
            ),
            None => (
                format!(
                    "search_web FAILED for query \"{query}\": no engine returned trustworthy \
                     results.\nattempts:\n- {}\nDo not retry the same query; rephrase once, or \
                     answer from your own knowledge and note that online verification failed.",
                    attempts.join("\n- ")
                ),
                false,
                "failed".to_string(),
            ),
        };

        let status = serde_json::json!({
            "engine_status": header,
            "success": success,
            "attempts": attempts,
        });
        call.turn_item_emitter
            .emit_completed(extension_turn_item(
                WebSearchItem {
                    id: call.call_id.clone(),
                    query: query.clone(),
                    action: Some(WebSearchAction::Search {
                        query: Some(query.clone()),
                        queries: None,
                    }),
                    results: Some(vec![status.clone()]),
                },
                EventMsg::WebSearchEnd(WebSearchEndEvent {
                    call_id: call.call_id.clone(),
                    query: query.clone(),
                    action: CoreWebSearchAction::Search {
                        query: Some(query.clone()),
                        queries: None,
                    },
                    results: Some(vec![status]),
                }),
            ))
            .await;

        let output = if success {
            WebToolOutput::ok(text)
        } else {
            WebToolOutput::failed(text)
        };
        Ok(Box::new(output))
    }
}

fn render_results(hits: &[SearchHit], header: &str) -> String {
    let mut out = format!("[search_web: {header}]\n");
    for (i, hit) in hits.iter().enumerate() {
        out.push_str(&format!("{}. {}\n   {}\n", i + 1, hit.title, hit.url));
        if !hit.snippet.is_empty() {
            out.push_str(&format!("   {}\n", truncate_chars(&hit.snippet, 300)));
        }
        if let Some(date) = hit.published.as_deref().filter(|d| !d.is_empty()) {
            out.push_str(&format!("   published: {date}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::SearchHit;
    use super::render_results;

    #[test]
    fn renders_ranked_results() {
        let hits = vec![
            SearchHit::new(
                "标题一".to_string(),
                "https://example.com/1".to_string(),
                "摘要一".to_string(),
            ),
            SearchHit::new(
                "标题二".to_string(),
                "https://example.com/2".to_string(),
                String::new(),
            ),
        ];
        let text = render_results(&hits, "bing direct+ensearch, kept 2/2 (attempt 1)");
        assert!(text.starts_with("[search_web: bing direct+ensearch"));
        assert!(text.contains("1. 标题一\n   https://example.com/1\n   摘要一\n"));
        assert!(text.contains("2. 标题二\n   https://example.com/2\n"));
    }
}
