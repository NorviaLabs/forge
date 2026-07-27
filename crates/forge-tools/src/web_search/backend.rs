use async_trait::async_trait;
use forge_config::{WebSearchConfig, WebSearchProvider};
use std::sync::Arc;
use thiserror::Error;

use super::mock::MockSearchBackend;

#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub num_results: u32,
    pub recency_days: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Injected at call time — never logged or placed in tool output.
#[derive(Debug, Clone, Default)]
pub struct SearchSecrets {
    pub api_key: Option<String>,
}

impl SearchSecrets {
    pub fn from_config(cfg: &WebSearchConfig) -> Self {
        let api_key = cfg.resolved_api_key_env().and_then(|name| {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        });
        Self { api_key }
    }
}

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("missing API key")]
    MissingKey,
    #[error("http error: {0}")]
    Http(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("timeout")]
    Timeout,
    #[error("{0}")]
    Other(String),
}

#[async_trait]
pub trait SearchBackend: Send + Sync {
    fn id(&self) -> &str;
    async fn search(
        &self,
        req: &SearchRequest,
        secrets: &SearchSecrets,
    ) -> Result<Vec<SearchHit>, SearchError>;
}

pub fn build_backend(provider: WebSearchProvider, _timeout_ms: u64) -> Arc<dyn SearchBackend> {
    match provider {
        WebSearchProvider::Mock => Arc::new(MockSearchBackend),
    }
}

/// Markdown + trailing JSON for model consumption.
pub fn format_search_results(original_query: &str, hits: &[SearchHit]) -> String {
    let mut out = format!("## Web search: {original_query}\n\n");
    if hits.is_empty() {
        out.push_str("No results.\n");
        return out;
    }
    for (i, h) in hits.iter().enumerate() {
        out.push_str(&format!(
            "{}. **{}**\n   - URL: {}\n   - Snippet: {}\n\n",
            i + 1,
            h.title,
            h.url,
            h.snippet
        ));
    }
    if let Ok(json) = serde_json::to_string_pretty(hits) {
        out.push_str("```json\n");
        out.push_str(&json);
        out.push_str("\n```\n");
    }
    out
}
