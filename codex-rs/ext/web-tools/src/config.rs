//! Runtime configuration for the web-tools extension.

use codex_core::config::Config;

pub(crate) const ENV_EXA_API_KEY: &str = "EXA_API_KEY";
pub(crate) const ENV_FIRECRAWL_API_KEY: &str = "FIRECRAWL_API_KEY";

pub(crate) const DEFAULT_SEARCH_MAX_RESULTS: u32 = 10;
pub(crate) const DEFAULT_FETCH_MAX_CHARS: usize = 30_000;
pub(crate) const DEFAULT_EXA_TIMEOUT_SECS: u64 = 20;
pub(crate) const DEFAULT_FIRECRAWL_TIMEOUT_SECS: u64 = 90;
pub(crate) const DEFAULT_BING_TIMEOUT_SECS: u64 = 25;

#[derive(Clone, Debug)]
pub(crate) struct WebToolsConfig {
    pub(crate) enabled: bool,
    pub(crate) exa_api_key: Option<String>,
    pub(crate) firecrawl_api_key: Option<String>,
    /// Optional explicit HTTP proxy used only for the Bing "change exit"
    /// retry rung (e.g. `http://127.0.0.1:2080`). Everything else is direct.
    pub(crate) bing_proxy: Option<String>,
    pub(crate) search_max_results: u32,
    pub(crate) fetch_max_chars: usize,
    pub(crate) exa_timeout: std::time::Duration,
    pub(crate) firecrawl_timeout: std::time::Duration,
    pub(crate) bing_timeout: std::time::Duration,
}

impl WebToolsConfig {
    pub(crate) fn fetch_available(&self) -> bool {
        self.enabled && (self.exa_api_key.is_some() || self.firecrawl_api_key.is_some())
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn env_key(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| non_empty(std::env::var(name).ok()))
}

impl From<&Config> for WebToolsConfig {
    fn from(config: &Config) -> Self {
        let toml = config.web_tools_config.as_ref();
        // Presence of `[tools.web_tools]` means "on" unless explicitly disabled.
        let enabled = toml.and_then(|toml| toml.enabled).unwrap_or(toml.is_some());
        let exa_api_key = toml
            .and_then(|toml| non_empty(toml.exa_api_key.clone()))
            .or_else(|| env_key(&[ENV_EXA_API_KEY]));
        let firecrawl_api_key = toml
            .and_then(|toml| non_empty(toml.firecrawl_api_key.clone()))
            .or_else(|| env_key(&[ENV_FIRECRAWL_API_KEY]));
        Self {
            enabled,
            exa_api_key,
            firecrawl_api_key,
            bing_proxy: toml.and_then(|toml| non_empty(toml.bing_proxy.clone())),
            search_max_results: toml
                .and_then(|toml| toml.search_max_results)
                .unwrap_or(DEFAULT_SEARCH_MAX_RESULTS)
                .clamp(1, 15),
            fetch_max_chars: toml
                .and_then(|toml| toml.fetch_max_chars)
                .unwrap_or(DEFAULT_FETCH_MAX_CHARS),
            exa_timeout: std::time::Duration::from_secs(
                toml.and_then(|toml| toml.exa_timeout_secs)
                    .unwrap_or(DEFAULT_EXA_TIMEOUT_SECS),
            ),
            firecrawl_timeout: std::time::Duration::from_secs(
                toml.and_then(|toml| toml.firecrawl_timeout_secs)
                    .unwrap_or(DEFAULT_FIRECRAWL_TIMEOUT_SECS),
            ),
            bing_timeout: std::time::Duration::from_secs(
                toml.and_then(|toml| toml.bing_timeout_secs)
                    .unwrap_or(DEFAULT_BING_TIMEOUT_SECS),
            ),
        }
    }
}
