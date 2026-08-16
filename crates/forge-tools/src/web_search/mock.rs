use async_trait::async_trait;

use super::backend::{SearchBackend, SearchError, SearchHit, SearchRequest, SearchSecrets};

/// Deterministic offline backend for tests. Never registered for users.
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

    #[test]
    fn id_reports_mock() {
        assert_eq!(MockSearchBackend.id(), "mock");
    }

    #[test]
    fn slugify_falls_back_to_query_when_no_alphanumerics_survive() {
        // Every character is stripped to `-` and then trimmed away entirely,
        // so the slug would otherwise be empty.
        assert_eq!(slugify("!!!"), "query");
        assert_eq!(slugify("---"), "query");
        assert_eq!(slugify(""), "query");
    }

    #[test]
    fn slugify_lowercases_and_truncates() {
        // Every non-alphanumeric maps to its own '-'; runs are not collapsed,
        // so the comma and the space each contribute one separator. Leading
        // and trailing separators are trimmed.
        assert_eq!(slugify("Hello, World!"), "hello--world");
        assert_eq!(slugify("simple"), "simple");
        // Long inputs are cut to the 48-character cap.
        let long = "a".repeat(100);
        assert_eq!(slugify(&long), "a".repeat(48));
    }
}
