use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::backend::{SearchBackend, SearchError, SearchHit, SearchRequest, SearchSecrets};

pub struct TavilyBackend {
    timeout: Duration,
    /// Override base URL for tests.
    base_url: String,
}

impl TavilyBackend {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            base_url: "https://api.tavily.com".into(),
        }
    }

    #[cfg(test)]
    pub fn with_base_url(timeout: Duration, base_url: impl Into<String>) -> Self {
        Self {
            timeout,
            base_url: base_url.into(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct TavilyResponse {
    #[serde(default)]
    results: Vec<TavilyResult>,
}

#[derive(Debug, Deserialize)]
struct TavilyResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
}

/// Parse Tavily JSON (unit-testable without network).
pub fn parse_tavily_response(body: &str, limit: u32) -> Result<Vec<SearchHit>, SearchError> {
    let parsed: TavilyResponse =
        serde_json::from_str(body).map_err(|e| SearchError::Provider(format!("invalid json: {e}")))?;
    let mut hits: Vec<SearchHit> = parsed
        .results
        .into_iter()
        .map(|r| SearchHit {
            title: r.title,
            url: r.url,
            snippet: r.content,
        })
        .collect();
    hits.truncate(limit.max(1) as usize);
    Ok(hits)
}

#[async_trait]
impl SearchBackend for TavilyBackend {
    fn id(&self) -> &str {
        "tavily"
    }

    async fn search(
        &self,
        req: &SearchRequest,
        secrets: &SearchSecrets,
    ) -> Result<Vec<SearchHit>, SearchError> {
        let key = secrets
            .api_key
            .as_deref()
            .filter(|k| !k.is_empty())
            .ok_or(SearchError::MissingKey)?;

        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|e| SearchError::Http(e.to_string()))?;

        let url = format!("{}/search", self.base_url.trim_end_matches('/'));
        let body = json!({
            "api_key": key,
            "query": req.query,
            "max_results": req.num_results,
            "include_answer": false,
        });

        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    SearchError::Timeout
                } else {
                    SearchError::Http(e.to_string())
                }
            })?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| SearchError::Http(e.to_string()))?;

        if !status.is_success() {
            // Do not echo body (may contain key material).
            return Err(SearchError::Provider(format!(
                "tavily HTTP {}",
                status.as_u16()
            )));
        }

        parse_tavily_response(&text, req.num_results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tavily_sample() {
        let body = r#"{
            "results": [
                {"title": "Rust", "url": "https://rust-lang.org", "content": "A language"},
                {"title": "Two", "url": "https://example.com", "content": "More"}
            ]
        }"#;
        let hits = parse_tavily_response(body, 1).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Rust");
        assert_eq!(hits[0].url, "https://rust-lang.org");
    }
}
