//! `fetch_web` tool: Exa first, Firecrawl fallback. No self-fetching
//! (benchmarks showed it loses on every axis; see spike/fetch_bench/report.md).

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

use crate::config::WebToolsConfig;
use crate::exa;
use crate::firecrawl;
use crate::output::WebToolOutput;
use crate::util::extension_turn_item;
use crate::util::truncate_bytes_boundary;

pub(crate) const TOOL_NAME: &str = "fetch_web";
/// Below this many chars a fetched page counts as a failure (empty-shell /
/// anti-bot pages) and the next provider is tried.
const MIN_OK_CHARS: usize = 500;
/// Provider-side request floor: display budget never shrinks the fetch budget.
const REQUEST_MIN_CHARS: usize = 30_000;

const DESCRIPTION: &str = "\
Fetch a public web page or PDF and return its main text content as markdown.

Routing is automatic: Exa is tried first, then Firecrawl (stronger on
anti-bot pages and PDFs; large PDFs can take ~60s).

Private/internal URLs (localhost, RFC1918, .local, ...) are refused: their
content must never be sent to third-party fetch providers. Pages behind
login or hard anti-bot walls may come back empty from every provider.

If the output says FAILED, every provider failed: do not retry the same URL.
Pick another source (e.g. via search_web) or ask the user to paste the
content.";

/// One provider's fetched document; `note` carries the provider's own error
/// when the text came back empty ("200 but empty" is Exa's failure mode).
pub(crate) struct FetchDoc {
    pub(crate) title: String,
    pub(crate) text: String,
    pub(crate) note: Option<String>,
}

impl FetchDoc {
    fn usable(&self) -> bool {
        !self.text.trim().is_empty() && self.text.chars().count() >= MIN_OK_CHARS
    }
}

pub(crate) struct FetchWebTool {
    pub(crate) config: WebToolsConfig,
    pub(crate) client: reqwest::Client,
}

#[derive(Deserialize)]
struct FetchArgs {
    url: String,
    max_chars: Option<usize>,
}

impl<'call> ToolExecutor<ToolCall<'call>> for FetchWebTool {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        let parameters = match parse_tool_input_schema(&serde_json::json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "Public http(s) URL to fetch."},
                "max_chars": {"type": "integer", "minimum": 1000,
                    "description": "Cap on returned characters. Default 30000."},
            },
            "required": ["url"],
            "additionalProperties": false,
        })) {
            Ok(parameters) => parameters,
            Err(err) => panic!("fetch_web schema should parse: {err}"),
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

impl FetchWebTool {
    async fn handle_call(
        &self,
        call: ToolCall<'_>,
    ) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let args: FetchArgs = serde_json::from_str(call.function_arguments()?)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        let url = args.url.trim();
        match url::Url::parse(url) {
            Ok(parsed)
                if matches!(parsed.scheme(), "http" | "https") && !is_private_url(&parsed) => {}
            Ok(_) => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "refused: `{url}` is not a public http(s) URL (private/internal addresses are never sent to fetch providers)"
                )));
            }
            Err(err) => return Err(FunctionCallError::RespondToModel(format!("invalid url: {err}"))),
        }

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

        let display_cap = args.max_chars.unwrap_or(self.config.fetch_max_chars);
        let request_chars = display_cap.max(REQUEST_MIN_CHARS);
        let mut attempts: Vec<String> = Vec::new();
        let mut best: Option<(&'static str, FetchDoc)> = None;

        if let Some(key) = &self.config.exa_api_key {
            match exa::fetch_contents(&self.client, key, url, request_chars, self.config.exa_timeout)
                .await
            {
                Ok(doc) if doc.usable() => best = Some(("exa", doc)),
                Ok(doc) => attempts.push(format!(
                    "exa: {}",
                    doc.note
                        .unwrap_or_else(|| format!("{} chars (below {MIN_OK_CHARS})", doc.text.chars().count()))
                )),
                Err(err) => attempts.push(format!("exa: {err:#}")),
            }
        }
        if best.is_none()
            && let Some(key) = &self.config.firecrawl_api_key
        {
            match firecrawl::scrape(&self.client, key, url, self.config.firecrawl_timeout).await {
                Ok(doc) if doc.usable() => best = Some(("firecrawl", doc)),
                Ok(doc) => attempts.push(format!(
                    "firecrawl: {}",
                    doc.note
                        .unwrap_or_else(|| format!("{} chars (below {MIN_OK_CHARS})", doc.text.chars().count()))
                )),
                Err(err) => attempts.push(format!("firecrawl: {err:#}")),
            }
        }

        let (text, success, provider, total_chars) = match best {
            Some((provider, doc)) => {
                let total = doc.text.chars().count();
                let budget = call.response_byte_budget(display_cap);
                let clipped = truncate_bytes_boundary(&doc.text, budget);
                let clipped = crate::util::truncate_chars(clipped, display_cap);
                let mut out = format!("[fetch_web: {url} via {provider}, {total} chars");
                if clipped.chars().count() < total {
                    out.push_str(&format!(", truncated to {display_cap}"));
                }
                out.push_str("]\n");
                if !doc.title.is_empty() {
                    out.push_str(&format!("# {}\n\n", doc.title));
                }
                out.push_str(&clipped);
                (out, true, provider.to_string(), total)
            }
            None => (
                format!(
                    "fetch_web FAILED for {url}: no provider returned usable content.\n\
                     attempts:\n- {}\n\
                     Do not retry the same URL; pick another source (e.g. via search_web) \
                     or ask the user to paste the content.",
                    attempts.join("\n- ")
                ),
                false,
                "none".to_string(),
                0,
            ),
        };

        let status = serde_json::json!({
            "provider": provider,
            "chars": total_chars,
            "success": success,
            "attempts": attempts,
        });
        call.turn_item_emitter
            .emit_completed(extension_turn_item(
                WebSearchItem {
                    id: call.call_id.clone(),
                    query: url.to_string(),
                    action: Some(WebSearchAction::OpenPage {
                        url: Some(url.to_string()),
                    }),
                    results: Some(vec![status.clone()]),
                },
                EventMsg::WebSearchEnd(WebSearchEndEvent {
                    call_id: call.call_id.clone(),
                    query: url.to_string(),
                    action: CoreWebSearchAction::OpenPage {
                        url: Some(url.to_string()),
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

/// Never send internal/private addresses to third-party fetch providers.
fn is_private_url(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(v4)) => {
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified()
        }
        Some(url::Host::Ipv6(v6)) => {
            let seg = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (seg & 0xffc0) == 0xfe80 // fe80::/10 link-local
                || (seg & 0xfe00) == 0xfc00 // fc00::/7 unique-local
        }
        Some(url::Host::Domain(domain)) => {
            let host = domain.to_lowercase();
            host.is_empty()
                || host == "localhost"
                || host.ends_with(".local")
                || host.ends_with(".localhost")
        }
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::is_private_url;

    #[test]
    fn private_urls_are_refused() {
        for url in [
            "http://localhost:8080/x",
            "http://127.0.0.1/x",
            "http://10.0.0.3/x",
            "http://172.16.1.1/x",
            "http://192.168.1.1/x",
            "http://169.254.1.1/x",
            "http://printer.local/",
            "http://[::1]/x",
            "http://[fe80::1]/x",
            "http://[fd12::1]/x",
        ] {
            let parsed = url::Url::parse(url).expect("test url parses");
            assert!(is_private_url(&parsed), "{url} should be private");
        }
    }

    #[test]
    fn public_urls_pass() {
        for url in [
            "https://help.aliyun.com/zh/x",
            "https://arxiv.org/abs/1706.03762",
            "http://example.com/page",
        ] {
            let parsed = url::Url::parse(url).expect("test url parses");
            assert!(!is_private_url(&parsed), "{url} should be public");
        }
    }
}
