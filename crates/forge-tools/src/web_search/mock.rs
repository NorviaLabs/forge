use async_trait::async_trait;

use super::backend::{SearchBackend, SearchError, SearchHit, SearchRequest, SearchSecrets};

/// Deterministic offline backend for CI and default installs.
pub struct MockSearchBackend;

#[async_trait]
impl SearchBackend for MockSearchBackend {
    fn id(&self) -> &str {
        "mock"
    }

    async fn search(
        &self,
        req: &SearchRequest,
        _secrets: &SearchSecrets,
    ) -> Result<Vec<SearchHit>, SearchError> {
        let n = req.num_results.clamp(1, 10) as usize;
        let slug = slugify(&req.query);
        let mut hits = Vec::with_capacity(n);
        for i in 0..n {
            hits.push(SearchHit {
                title: format!("Mock result {} for \"{}\"", i + 1, req.query),
                url: format!("https://example.com/search/{slug}/{}", i + 1),
                snippet: format!(
                    "Deterministic mock snippet #{} about {} (Forge web_search mock backend).",
                    i + 1,
                    req.query
                ),
            });
        }
        Ok(hits)
    }
}

fn slugify(q: &str) -> String {
    let s: String = q
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "query".into()
    } else {
        s.chars().take(48).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_is_deterministic() {
        let b = MockSearchBackend;
        let req = SearchRequest {
            query: "hello world".into(),
            num_results: 2,
            recency_days: None,
        };
        let a = b.search(&req, &SearchSecrets::default()).await.unwrap();
        let c = b.search(&req, &SearchSecrets::default()).await.unwrap();
        assert_eq!(a, c);
        assert_eq!(a.len(), 2);
        assert!(a[0].url.contains("hello-world"));
    }
}
