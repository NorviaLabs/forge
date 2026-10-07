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
        return "Model error: authentication failed. Run /connect to review this provider's credentials."
            .into();
    }
    if lower.contains("validation")
        || lower.contains("schema")
        || lower.contains("invalid tool")
        || lower.contains("invalid argument")
    {
        let detail = error_detail(raw);
        return format!("Invalid tool request: {detail}\nReview the request before retrying.");
    }
    if lower.contains("timeout") || lower.contains("timed out") {
        return "Model error: request timed out. Retry or check the provider endpoint.".into();
    }
    let detail = error_detail(raw);
    if detail.is_empty() {
        "Operation failed.".into()
    } else {
        format!("Operation failed: {detail}")
    }
}

/// Retain inspectable, bounded details in the transcript after the toast ends.
/// Known credential errors above deliberately omit their raw payload.
fn error_detail(raw: &str) -> String {
    let mut chars = raw.chars();
    let mut detail: String = chars.by_ref().take(4096).collect();
    if chars.next().is_some() {
        detail.push_str("\n[Error details truncated]");
    }
    crate::decision::visible_text(&detail)
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
        assert!(message.contains("Invalid tool request"));
        assert!(message.contains("Review the request"));
        assert!(!message.contains("no files were changed"));
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

    #[test]
    fn error_details_keep_the_tail_and_show_control_characters_without_execution() {
        let message = classify_operator_error(&format!(
            "{}\nuseful tail\u{1b}[2J",
            "failure detail ".repeat(30)
        ));
        assert!(message.contains("useful tail"));
        assert!(!message.contains('\u{1b}'));
        assert!(message.contains("\\u{1b}"));
        assert!(classify_operator_error(&"λ".repeat(5000)).contains("[Error details truncated]"));
        assert!(!classify_operator_error("401 api_key=secret sk-private").contains("secret"));
        assert!(
            !classify_operator_error("schema invalid api_key=secret sk-private").contains("secret")
        );
    }
}
