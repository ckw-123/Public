//! Extension wiring: config lifecycle + tool contribution.

use std::sync::Arc;

use codex_core::config::Config;
use codex_extension_api::ConfigContributor;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::ThreadLifecycleContributor;
use codex_extension_api::ThreadStartInput;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolContributor;
use codex_extension_api::ToolExecutor;

use crate::bing::USER_AGENT;
use crate::config::WebToolsConfig;
use crate::fetch::FetchWebTool;
use crate::search::SearchWebTool;

#[derive(Clone)]
struct WebToolsExtension;

#[derive(Clone)]
struct WebToolsExtensionConfig {
    config: WebToolsConfig,
    client: reqwest::Client,
}

impl From<&Config> for WebToolsExtensionConfig {
    fn from(config: &Config) -> Self {
        // `no_proxy`: engine routing is deliberately deterministic (direct by
        // default, explicit `bing_proxy` for the proxy rung); a system proxy
        // must not silently reroute Bing and corrupt route attribution.
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .no_proxy()
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config: WebToolsConfig::from(config),
            client,
        }
    }
}

impl ThreadLifecycleContributor<Config> for WebToolsExtension {
    fn on_thread_start<'a>(
        &'a self,
        input: ThreadStartInput<'a, Config>,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            input
                .thread_store
                .insert(WebToolsExtensionConfig::from(input.config));
        })
    }
}

impl ConfigContributor<Config> for WebToolsExtension {
    fn on_config_changed(
        &self,
        _session_store: &ExtensionData,
        thread_store: &ExtensionData,
        _previous_config: &Config,
        new_config: &Config,
    ) {
        thread_store.insert(WebToolsExtensionConfig::from(new_config));
    }
}

impl ToolContributor for WebToolsExtension {
    fn tools(
        &self,
        _session_store: &ExtensionData,
        thread_store: &ExtensionData,
    ) -> Vec<Arc<dyn for<'call> ToolExecutor<ToolCall<'call>>>> {
        let Some(extension_config) = thread_store.get::<WebToolsExtensionConfig>() else {
            return Vec::new();
        };
        let config = extension_config.config.clone();
        if !config.enabled {
            return Vec::new();
        }
        let client = extension_config.client.clone();
        let mut tools: Vec<Arc<dyn for<'call> ToolExecutor<ToolCall<'call>>>> =
            vec![Arc::new(SearchWebTool {
                config: config.clone(),
                client: client.clone(),
            })];
        if config.fetch_available() {
            tools.push(Arc::new(FetchWebTool { config, client }));
        }
        tools
    }
}

/// Install the web-tools extension. Unlike the upstream web-search extension
/// this needs no model-provider auth: keys come from config/env at runtime.
pub fn install(registry: &mut ExtensionRegistryBuilder<Config>) {
    let extension = Arc::new(WebToolsExtension);
    registry.thread_lifecycle_contributor(extension.clone());
    registry.config_contributor(extension.clone());
    registry.tool_contributor(extension);
}
