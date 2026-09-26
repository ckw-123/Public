//! Built-in web search & fetch tools for model providers without a hosted
//! web tool (e.g. third-party Responses API providers).
//!
//! Two model-facing tools are contributed when `[tools.web_tools]` is present
//! in `config.toml`:
//!
//! - `search_web`: Bing direct (`cn.bing.com` + `ensearch=1`) with
//!   signature-based degradation detection and same-URL retries; an optional
//!   proxy rung and then Exa act as fallbacks when configured.
//! - `fetch_web`: Exa `/contents` first, Firecrawl `/scrape` as fallback
//!   (stronger on anti-bot pages and PDFs).
//!
//! Design source: `spike/bing_findings.md` and `spike/fetch_bench/report.md`.
//! API keys are read at runtime from `[tools.web_tools]` config values or the
//! `EXA_API_KEY` / `FIRECRAWL_API_KEY` environment variables; they never
//! enter this repository.

mod bing;
mod config;
mod degrade;
mod exa;
mod extension;
mod fetch;
mod firecrawl;
mod output;
mod search;
mod util;

pub use extension::install;
