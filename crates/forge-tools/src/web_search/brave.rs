use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use super::backend::{SearchBackend, SearchError, SearchHit, SearchRequest, SearchSecrets};

pub struct BraveBackend {
    timeout: Duration,
    base_url: String,
}

impl BraveBackend {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            base_url: "https://api.search.brave.com".into(),
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
struct BraveResponse {
    #[serde(default)]
    web: Option<BraveWeb>,
}

#[derive(Debug, Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Debug, Deserialize)]
struct BraveResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    description: String,
}

pub fn parse_brave_response(body: &str, limit: u32) -> Result<Vec<SearchHit>, SearchError> {
    let parsed: BraveResponse = serde_json::from_str(body)
        .map_err(|e| SearchError::Provider(format!("invalid json: {e}")))?;
    let mut hits: Vec<SearchHit> = parsed
        .web
        .map(|w| w.results)
        .unwrap_or_default()
        .into_iter()
        .map(|r| SearchHit {
            title: r.title,
            url: r.url,
            snippet: r.description,
        })
        .collect();
    hits.truncate(limit.max(1) as usize);
    Ok(hits)
}

#[async_trait]
impl SearchBackend for BraveBackend {
    fn id(&self) -> &str {
        "brave"
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

        let url = format!("{}/res/v1/web/search", self.base_url.trim_end_matches('/'));

        let resp = client
            .get(&url)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", key)
            .query(&[
                ("q", req.query.as_str()),
                ("count", &req.num_results.to_string()),
            ])
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
            return Err(SearchError::Provider(format!(
                "brave HTTP {}",
                status.as_u16()
            )));
        }

        parse_brave_response(&text, req.num_results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_brave_sample() {
        let body = r#"{
            "web": {
                "results": [
                    {"title": "Brave", "url": "https://brave.com", "description": "Search"}
                ]
            }
        }"#;
        let hits = parse_brave_response(body, 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Brave");
        assert_eq!(hits[0].snippet, "Search");
    }
}
