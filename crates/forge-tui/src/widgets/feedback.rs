//! Severity labels and error classification shared by toast notifications.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FeedbackSeverity {
    #[default]
    Info,
    Warn,
    Error,
    Ok,
}

#[derive(Debug, Clone, Default)]
pub struct FeedbackModel {
    pub text: String,
    pub severity: FeedbackSeverity,
}

impl FeedbackModel {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            severity: FeedbackSeverity::Info,
        }
    }

    pub fn warn(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            severity: FeedbackSeverity::Warn,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            severity: FeedbackSeverity::Error,
        }
    }

    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            severity: FeedbackSeverity::Ok,
        }
    }
}

/// Map raw errors to operator-facing copy (TUI-08).
pub fn classify_operator_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("validation")
        || lower.contains("schema")
        || lower.contains("invalid tool")
        || lower.contains("invalid argument")
    {
        let trimmed: String = raw.chars().take(200).collect();
        return format!(
            "Correcting an invalid tool request: {trimmed}. No command was executed and no files were changed."
        );
    }
    if lower.contains("429") || lower.contains("rate limit") || lower.contains("rate_limit") {
        return "Model error: rate limited (HTTP 429). Wait and retry, or /model.".into();
    }
    if lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("authentication")
        || lower.contains("api key")
        || lower.contains("api_key")
        || lower.contains("unauthenticated")
        || lower.contains("no credentials")
        || lower.contains("bearer ")
        || lower.contains("sk-")
        || lower.contains("secret=")
        || lower.contains("fixture token")
        || lower.contains("fixture-")
    {
        return "Model error: authentication failed. Run /connect xai and finish real OAuth \
(not fixture), or set XAI_API_KEY."
            .into();
    }
    if lower.contains("timeout") || lower.contains("timed out") {
        return "Model error: request timed out. Retry or check the provider endpoint.".into();
    }
    let trimmed: String = raw.chars().take(200).collect();
    if trimmed.is_empty() {
        "Operation failed.".into()
    } else {
        format!("Model error: {trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_rate_limit() {
        let s = classify_operator_error("upstream returned 429 rate limit exceeded");
        assert!(s.contains("429"));
        assert!(s.contains("rate limited"));
    }

    #[test]
    fn classify_auth() {
        assert!(classify_operator_error("401 unauthorized").contains("authentication"));
        let message = classify_operator_error("failed api_key=secret sk-private");
        assert!(message.contains("authentication"));
        assert!(!message.contains("secret"));
        assert!(!message.contains("sk-private"));
    }

    #[test]
    fn classify_validation() {
        let message = classify_operator_error("schema validation failed: path is required");
        assert!(message.contains("invalid tool request"));
        assert!(message.contains("No command was executed"));
    }

    #[test]
    fn legacy_worker_errors_do_not_suggest_python_install() {
        let message = classify_operator_error("worker unavailable: no module named litellm");
        assert!(!message.contains("pip install"));
        assert!(!message.contains("forge-litellm-worker"));
    }

    #[test]
    fn empty_feedback() {
        assert!(FeedbackModel::default().is_empty());
        assert!(!FeedbackModel::error("x").is_empty());
    }

    #[test]
    fn constructors_set_expected_severity() {
        assert_eq!(FeedbackModel::info("i").severity, FeedbackSeverity::Info);
        assert_eq!(FeedbackModel::warn("w").severity, FeedbackSeverity::Warn);
        assert_eq!(FeedbackModel::error("e").severity, FeedbackSeverity::Error);
        assert_eq!(FeedbackModel::ok("o").severity, FeedbackSeverity::Ok);
    }

    #[test]
    fn classify_timeout_and_generic_errors() {
        assert!(classify_operator_error("request timed out").contains("timed out"));
        assert_eq!(classify_operator_error(""), "Operation failed.");
        assert!(classify_operator_error("boom").contains("boom"));
    }
}
