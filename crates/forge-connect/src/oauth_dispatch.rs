use crate::auth::{OauthPending, OauthTokens};
use crate::oauth_openai_codex::{OpenAiCodexOauthClient, OpenAiCodexOauthError};
use crate::oauth_xai::{XaiOauthClient, XaiOauthError};
use crate::profile::ConnectProfile;

pub(crate) enum PollResult {
    Pending,
    SlowDown,
    Complete(OauthTokens),
}

use thiserror::Error;

/// An OAuth device-code step failed.
///
/// Previously `Result<_, String>`: the provider clients already return typed
/// errors, and their `Display` was flattened here. This keeps the wording while
/// letting callers see the category.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OauthError {
    /// A provider OAuth client reported a failure.
    #[error("{0}")]
    Provider(String),
    /// The profile is not configured for OAuth.
    #[error("profile `{0}` is not OAuth")]
    NotOauth(String),
    /// No structured category applies.
    #[error("{0}")]
    Message(String),
}

pub(crate) struct OauthDispatcher;

impl OauthDispatcher {
    pub(crate) fn start(profile: &ConnectProfile) -> Result<OauthPending, OauthError> {
        if profile.id == crate::openai_codex::PROFILE_ID {
            return OpenAiCodexOauthClient::start_device_code()
                .map_err(|error| OauthError::Provider(error.to_string()));
        }

        Self::xai_client(profile)?
            .start_device_code(&profile.id)
            .map_err(|error| OauthError::Provider(error.to_string()))
    }

    pub(crate) fn poll(pending: &OauthPending) -> Result<PollResult, OauthError> {
        if pending.profile_id == crate::openai_codex::PROFILE_ID {
            return match OpenAiCodexOauthClient::poll_token_once(pending) {
                Ok(tokens) => Ok(PollResult::Complete(tokens)),
                Err(OpenAiCodexOauthError::Pending) => Ok(PollResult::Pending),
                Err(OpenAiCodexOauthError::SlowDown) => Ok(PollResult::SlowDown),
                Err(error) => Err(OauthError::Provider(error.to_string())),
            };
        }

        let client = XaiOauthClient {
            issuer: pending.auth_server.clone(),
            client_id: pending.client_id.clone(),
            ..XaiOauthClient::from_env()
        };
        match client.poll_token_once(pending) {
            Ok(tokens) => Ok(PollResult::Complete(tokens)),
            Err(XaiOauthError::AuthorizationPending) => Ok(PollResult::Pending),
            Err(XaiOauthError::SlowDown) => Ok(PollResult::SlowDown),
            Err(error) => Err(OauthError::Provider(error.to_string())),
        }
    }

    pub(crate) fn refresh(
        profile: &ConnectProfile,
        refresh_token: &str,
    ) -> Result<OauthTokens, OauthError> {
        if profile.id == crate::openai_codex::PROFILE_ID {
            return OpenAiCodexOauthClient::refresh(refresh_token)
                .map_err(|error| OauthError::Provider(error.to_string()));
        }

        Self::xai_client(profile)?
            .refresh_access_token(refresh_token)
            .map_err(|error| OauthError::Provider(error.to_string()))
    }

    fn xai_client(profile: &ConnectProfile) -> Result<XaiOauthClient, OauthError> {
        let crate::auth::AuthMode::Oauth { auth_server, .. } = &profile.auth_mode else {
            return Err(OauthError::NotOauth(profile.id.clone()));
        };
        let client = XaiOauthClient::from_env();
        if profile.id == crate::xai::PROFILE_ID {
            Ok(client)
        } else {
            Ok(XaiOauthClient {
                issuer: auth_server.clone(),
                ..client
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthMode;

    fn profile(id: &str, auth_mode: AuthMode) -> ConnectProfile {
        ConnectProfile {
            id: id.into(),
            title: "Test".into(),
            description: String::new(),
            auth_mode,
            api_key_env: vec![],
            default_base_url: None,
            default_models: vec![],
            models_dev_providers: vec![],
            auth_url: None,
            model_provider_prefix: id.into(),
        }
    }

    #[test]
    fn xai_client_rejects_api_key_profiles() {
        let profile = profile(
            "demo",
            AuthMode::ApiKey {
                tui_always_prompt: false,
            },
        );
        assert!(OauthDispatcher::xai_client(&profile)
            .unwrap_err()
            .to_string()
            .contains("not OAuth"));
    }

    #[test]
    fn xai_client_uses_profile_issuer_for_generic_oauth() {
        let profile = profile(
            "generic",
            AuthMode::Oauth {
                device_code: true,
                system_browser: false,
                auth_server: "https://issuer.example".into(),
            },
        );
        let client = OauthDispatcher::xai_client(&profile).unwrap();
        assert_eq!(client.issuer, "https://issuer.example");
    }

    #[test]
    fn xai_profile_keeps_environment_client_defaults() {
        let profile = crate::xai::xai_grok_profile();
        let client = OauthDispatcher::xai_client(&profile).unwrap();
        assert!(!client.client_id.is_empty());
        assert!(!client.issuer.is_empty());
    }

    /// `start` and `refresh` both consult `xai_client` before making any
    /// request, so a non-OAuth profile is rejected without network access.
    /// These assert that short-circuit rather than the transport behind it.
    #[test]
    fn start_rejects_a_non_oauth_profile_without_contacting_a_server() {
        let profile = profile(
            "demo",
            AuthMode::ApiKey {
                tui_always_prompt: false,
            },
        );
        let err = OauthDispatcher::start(&profile).unwrap_err();
        assert!(
            matches!(err, OauthError::NotOauth(ref id) if id == "demo"),
            "expected NotOauth(\"demo\"), got {err:?}"
        );
        assert_eq!(err.to_string(), "profile `demo` is not OAuth");
    }

    #[test]
    fn refresh_rejects_a_non_oauth_profile_without_contacting_a_server() {
        let profile = profile(
            "demo",
            AuthMode::ApiKey {
                tui_always_prompt: false,
            },
        );
        let err = OauthDispatcher::refresh(&profile, "rt").unwrap_err();
        assert!(matches!(err, OauthError::NotOauth(ref id) if id == "demo"));
    }

    #[test]
    fn oauth_error_display_is_stable() {
        assert_eq!(
            OauthError::Provider("upstream said no".into()).to_string(),
            "upstream said no"
        );
        assert_eq!(
            OauthError::NotOauth("demo".into()).to_string(),
            "profile `demo` is not OAuth"
        );
        assert_eq!(OauthError::Message("plain".into()).to_string(), "plain");
    }
}
