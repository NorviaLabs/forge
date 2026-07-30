//! Phase 9 — `web_search` built-in (WEB-01).
//!
//! Backend selection and the `SearchBackend` extension point are documented in
//! `docs/architecture.md`; the design doc this used to reference was deleted.

mod backend;
mod mock;

pub use backend::{
    format_search_results, SearchBackend, SearchError, SearchHit, SearchRequest, SearchSecrets,
};
pub use mock::MockSearchBackend;

use async_trait::async_trait;
use forge_config::WebSearchConfig;
use forge_types::{SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::registry::ToolContext;
use crate::{Tool, ToolError};

use backend::build_backend;

fn schema_for<T: JsonSchema>() -> Value {
    let s = schemars::schema_for!(T);
    serde_json::to_value(s).unwrap_or_else(|_| json!({"type": "object"}))
}

/// Model-facing input for `web_search`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WebSearchArgs {
    /// Search query (non-empty).
    pub query: String,
    /// Desired number of results (clamped to config max).
    #[serde(default)]
    pub num_results: Option<u32>,
    /// Optional site filter (mapped to `site:` when supported).
    #[serde(default)]
    pub site: Option<String>,
    /// Optional freshness hint in days (backends may ignore).
    #[serde(default)]
    pub recency_days: Option<u32>,
}

/// Built-in web search tool.
pub struct WebSearchTool {
    backend: Arc<dyn SearchBackend>,
    cfg: WebSearchConfig,
}

impl WebSearchTool {
    /// Build a tool if config says it should be registered.
    pub fn try_new(cfg: &WebSearchConfig) -> Option<Self> {
        if !cfg.should_register() {
            return None;
        }
        let backend = build_backend(cfg.provider, cfg.timeout_ms);
        Some(Self {
            backend,
            cfg: cfg.clone(),
        })
    }

    /// Force-construct for tests (ignores should_register).
    pub fn new_for_tests(cfg: WebSearchConfig, backend: Arc<dyn SearchBackend>) -> Self {
        Self { backend, cfg }
    }

    fn clamp_num_results(&self, requested: Option<u32>) -> u32 {
        let max = self.cfg.max_results.max(1);
        let n = requested.unwrap_or(5).max(1);
        n.min(max)
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the public web for documentation, APIs, package versions, and references. \
         Returns ranked results with title, URL, and snippet."
    }

    fn input_schema(&self) -> Value {
        schema_for::<WebSearchArgs>()
    }

    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Network
    }

    fn idempotent(&self) -> bool {
        true
    }

    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: WebSearchArgs = serde_json::from_value(args).map_err(|e| {
            ToolError::Execution(format!("internal deserialize after validation: {e}"))
        })?;

        let query = a.query.trim().to_string();
        if query.is_empty() {
            return Ok(ToolOutput {
                content: "web_search: query must be non-empty".into(),
                is_error: true,
            });
        }
        if query.chars().count() > self.cfg.max_query_chars as usize {
            return Ok(ToolOutput {
                content: format!(
                    "web_search: query exceeds max length ({} chars)",
                    self.cfg.max_query_chars
                ),
                is_error: true,
            });
        }

        let secrets = SearchSecrets::from_config(&self.cfg);
        if self.cfg.provider.needs_api_key() && secrets.api_key.is_none() {
            return Ok(ToolOutput {
                content: "web_search not configured (missing API key)".into(),
                is_error: true,
            });
        }

        let mut q = query.clone();
        if let Some(site) = a.site.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            if !q.contains("site:") {
                q = format!("{q} site:{site}");
            }
        }

        let req = SearchRequest {
            query: q,
            num_results: self.clamp_num_results(a.num_results),
            recency_days: a.recency_days,
        };

        match self.backend.search(&req, &secrets).await {
            Ok(hits) => {
                let content = format_search_results(&query, &hits);
                // Never include secrets in content (defensive).
                if let Some(ref key) = secrets.api_key {
                    if !key.is_empty() && content.contains(key) {
                        return Ok(ToolOutput {
                            content: "web_search: redacted unexpected secret in result".into(),
                            is_error: true,
                        });
                    }
                }
                Ok(ToolOutput {
                    content,
                    is_error: false,
                })
            }
            Err(e) => Ok(ToolOutput {
                content: format!("web_search failed: {e}"),
                is_error: true,
            }),
        }
    }
}

/// Whether this config should expose `web_search` to the model.
pub fn should_register_web_search(cfg: &WebSearchConfig) -> bool {
    cfg.should_register()
}

/// Optionally produce a registered `web_search` tool from config.
pub fn web_search_tool(cfg: &WebSearchConfig) -> Option<Arc<dyn Tool>> {
    WebSearchTool::try_new(cfg).map(|t| Arc::new(t) as Arc<dyn Tool>)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::validate_args;
    use crate::ToolRegistry;
    use crate::ValidationBudget;
    use serde_json::json;
    #[test]
    fn schema_rejects_missing_query() {
        let cfg = WebSearchConfig::default();
        let tool = WebSearchTool::try_new(&cfg).expect("mock registers");
        let err = validate_args("web_search", &tool.input_schema(), &json!({})).unwrap_err();
        assert_eq!(err.tool, "web_search");
    }

    #[test]
    fn schema_accepts_query() {
        let cfg = WebSearchConfig::default();
        let tool = WebSearchTool::try_new(&cfg).unwrap();
        validate_args(
            "web_search",
            &tool.input_schema(),
            &json!({"query": "rust async trait"}),
        )
        .unwrap();
    }

    #[test]
    fn try_new_none_when_disabled() {
        let cfg = WebSearchConfig {
            enabled: false,
            ..Default::default()
        };
        assert!(WebSearchTool::try_new(&cfg).is_none());
    }

    #[tokio::test]
    async fn mock_search_returns_hits() {
        let cfg = WebSearchConfig::default();
        let tool = WebSearchTool::try_new(&cfg).unwrap();
        let ctx = ToolContext::new(std::env::current_dir().unwrap());
        let out = tool
            .call(
                &ctx,
                json!({"query": "forge agent harness", "num_results": 3}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Web search:"));
        assert!(out.content.contains("forge agent harness"));
        assert!(out.content.contains("http"));
        assert!(!out.content.contains("TAVILY"));
        assert!(!out.content.contains("api_key"));
    }

    #[tokio::test]
    async fn empty_query_is_error_after_validation_bypass() {
        // Empty string passes schemars string type but tool rejects.
        let cfg = WebSearchConfig::default();
        let tool = WebSearchTool::try_new(&cfg).unwrap();
        let ctx = ToolContext::new(std::env::current_dir().unwrap());
        let out = tool.call(&ctx, json!({"query": "   "})).await.unwrap();
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn registry_call_validates_then_executes() {
        let cfg = WebSearchConfig::default();
        let mut reg = ToolRegistry::new();
        reg.register(web_search_tool(&cfg).unwrap());
        let ctx = ToolContext::new(std::env::current_dir().unwrap());
        let mut budget = ValidationBudget::with_default_max();
        let out = reg
            .call(
                &ctx,
                "web_search",
                json!({"query": "serde json schema"}),
                &mut budget,
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("serde"));
    }

    #[tokio::test]
    async fn registry_blocks_invalid_args() {
        let cfg = WebSearchConfig::default();
        let mut reg = ToolRegistry::new();
        reg.register(web_search_tool(&cfg).unwrap());
        let ctx = ToolContext::new(std::env::current_dir().unwrap());
        let mut budget = ValidationBudget::with_default_max();
        let err = reg
            .call(&ctx, "web_search", json!({"query": 1}), &mut budget)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[test]
    fn side_effect_is_network() {
        let tool = WebSearchTool::try_new(&WebSearchConfig::default()).unwrap();
        assert_eq!(tool.side_effect_class(), SideEffectClass::Network);
        assert!(tool.idempotent());
    }

    #[test]
    fn format_results_markdown() {
        let hits = vec![SearchHit {
            title: "Example".into(),
            url: "https://example.com".into(),
            snippet: "Hello".into(),
        }];
        let md = format_search_results("q", &hits);
        assert!(md.contains("## Web search: q"));
        assert!(md.contains("**Example**"));
        assert!(md.contains("https://example.com"));
        assert!(md.contains("```json"));
    }

    #[test]
    fn format_empty_hits() {
        let md = format_search_results("nothing", &[]);
        assert!(md.contains("No results"));
    }
}
