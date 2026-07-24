use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::backend::{SearchBackend, SearchError, SearchHit, SearchRequest, SearchSecrets};

pub struct SerperBackend {
    timeout: Duration,
    base_url: String,
}

impl SerperBackend {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            base_url: "https://google.serper.dev".into(),
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
struct SerperResponse {
    #[serde(default)]
    organic: Vec<SerperOrganic>,
}

#[derive(Debug, Deserialize)]
struct SerperOrganic {
    #[serde(default)]
    title: String,
    #[serde(default)]
    link: String,
    #[serde(default)]
    snippet: String,
}

pub fn parse_serper_response(body: &str, limit: u32) -> Result<Vec<SearchHit>, SearchError> {
    let parsed: SerperResponse = serde_json::from_str(body)
        .map_err(|e| SearchError::Provider(format!("invalid json: {e}")))?;
    let mut hits: Vec<SearchHit> = parsed
        .organic
        .into_iter()
        .map(|r| SearchHit {
            title: r.title,
            url: r.link,
            snippet: r.snippet,
        })
        .collect();
    hits.truncate(limit.max(1) as usize);
    Ok(hits)
}

#[async_trait]
impl SearchBackend for SerperBackend {
    fn id(&self) -> &str {
        "serper"
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
            "q": req.query,
            "num": req.num_results,
        });

        let resp = client
            .post(&url)
            .header("X-API-KEY", key)
            .header("Content-Type", "application/json")
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
            return Err(SearchError::Provider(format!(
                "serper HTTP {}",
                status.as_u16()
            )));
        }

        parse_serper_response(&text, req.num_results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web_search::test_support::serve_once;

    #[test]
    fn parse_serper_sample() {
        let body = r#"{
            "organic": [
                {"title": "Google", "link": "https://google.com", "snippet": "Search engine"}
            ]
        }"#;
        let hits = parse_serper_response(body, 3).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://google.com");
        assert_eq!(hits[0].snippet, "Search engine");
    }

    #[tokio::test]
    async fn search_sends_json_header_and_parses_response() {
        let (base_url, request) = serve_once(
            "200 OK",
            r#"{"organic":[{"title":"Result","link":"https://example.com","snippet":"Found"}]}"#,
        )
        .await;
        let backend = SerperBackend::with_base_url(Duration::from_secs(1), base_url);
        let hits = backend
            .search(
                &SearchRequest {
                    query: "forge".into(),
                    num_results: 3,
                    recency_days: None,
                },
                &SearchSecrets {
                    api_key: Some("serper-secret".into()),
                },
            )
            .await
            .unwrap();

        assert_eq!(hits[0].url, "https://example.com");
        let request = request.await.unwrap();
        assert!(request.starts_with("POST /search HTTP/1.1"));
        assert!(request
            .to_ascii_lowercase()
            .contains("x-api-key: serper-secret"));
        assert!(request.contains(r#""q":"forge""#));
        assert!(request.contains(r#""num":3"#));
    }
}
