//! OpenAI connect profile — API key (`openai/*`).

use crate::profile::ProviderSpec;

pub const PROFILE_ID: &str = crate::builtin::profile_ids::OPENAI;

/// The embedded built-in definition (`providers/*.toml`).
pub fn openai_profile() -> ProviderSpec {
    crate::builtin::builtin_profile(PROFILE_ID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::VerifyError;

    /// The profile's key check, against `base`.
    fn verify_api_key(key: &str, base: &str) -> Result<(), crate::verify::VerifyError> {
        let mut profile = openai_profile();
        profile.base_url = base.into();
        crate::service::verify_profile_key(&profile, key)
    }

    fn mock_server(status: u16) -> Option<String> {
        crate::test_support::serve(vec![status], r#"{"data":[]}"#)
    }

    #[test]
    fn profile_shape() {
        let p = openai_profile();
        assert_eq!(p.id, "openai");
        assert!(p.needs_tui_api_key_prompt());
        assert!(p.default_models.iter().all(|m| m.starts_with("openai/")));
    }

    #[test]
    fn short_key_rejected() {
        let err = verify_api_key("sk-short", &openai_profile().base_url)
            .unwrap_err()
            .to_string();
        assert!(err.contains("too short"), "{err}");
    }

    #[test]
    fn empty_key_rejected_without_network() {
        let err = verify_api_key("   ", &openai_profile().base_url)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "API key is empty");
    }

    #[test]
    fn verifies_success_with_trimmed_base_url() {
        let Some(base) = mock_server(200) else {
            eprintln!("skipping: this host denies binding a mock listener");
            return;
        };
        assert!(verify_api_key("sk-valid-key-for-tests", &base).is_ok());
    }

    #[test]
    fn verify_reports_auth_and_server_statuses_without_secret() {
        let Some(base) = mock_server(401) else {
            eprintln!("skipping: this host denies binding a mock listener");
            return;
        };
        let err = verify_api_key("sk-valid-key-for-tests", &base)
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 401"), "{err}");
        assert!(!err.contains("sk-valid"));

        let err = verify_api_key("sk-valid-key-for-tests", &mock_server(500).unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 500"), "{err}");
        assert!(!err.contains("sk-valid"));
    }

    /// The key goes only to the configured host: a redirect is reported,
    /// never followed. (Following it here would reach a closed port and show
    /// up as `Unreachable`.)
    #[test]
    fn verify_does_not_follow_a_redirect() {
        let Some(base) = forge_test_support::mock_http(vec![(
            302,
            "",
            vec![("Location", "http://127.0.0.1:9/models")],
        )]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        let error = verify_api_key("sk-0123456789abcdef0123", &base).unwrap_err();
        assert!(
            matches!(error, VerifyError::Status { status: 302, .. }),
            "{error:?}"
        );
    }
}
