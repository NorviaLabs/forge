use crate::auth::{OauthPending, OauthTokens};
use crate::oauth_chatgpt::{ChatgptOauthClient, ChatgptOauthError};
use crate::oauth_rfc8628::{Rfc8628Client, Rfc8628Error};
use crate::profile::{ConnectProfile, OauthFlavor};

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

/// The device-login flavor a profile signs in with (design §1.1
/// `auth.flavor`), or `NotOauth`.
fn flavor(profile: &ConnectProfile) -> Result<OauthFlavor, OauthError> {
    let auth = profile.effective_auth();
    match auth.flavor {
        Some(flavor) if auth.is_oauth() => Ok(flavor),
        _ => Err(OauthError::NotOauth(profile.id.clone())),
    }
}

fn rfc8628(profile: &ConnectProfile) -> Result<Rfc8628Client, OauthError> {
    Rfc8628Client::for_profile(profile).ok_or_else(|| {
        OauthError::Message(format!(
            "profile `{}` needs auth.issuer and auth.client_id for device login",
            profile.id
        ))
    })
}

impl OauthDispatcher {
    pub(crate) fn start(profile: &ConnectProfile) -> Result<OauthPending, OauthError> {
        match flavor(profile)? {
            OauthFlavor::Chatgpt => ChatgptOauthClient::for_profile(profile)
                .start_device_code(&profile.id)
                .map_err(|error| OauthError::Provider(error.to_string())),
            OauthFlavor::Rfc8628 => rfc8628(profile)?
                .start_device_code(&profile.id)
                .map_err(|error| OauthError::Provider(error.to_string())),
        }
    }

    pub(crate) fn poll(pending: &OauthPending) -> Result<PollResult, OauthError> {
        match pending.flavor {
            OauthFlavor::Chatgpt => {
                let client = ChatgptOauthClient {
                    auth_base: pending.auth_server.clone(),
                };
                match client.poll_token_once(pending) {
                    Ok(tokens) => Ok(PollResult::Complete(tokens)),
                    Err(ChatgptOauthError::Pending) => Ok(PollResult::Pending),
                    Err(ChatgptOauthError::SlowDown) => Ok(PollResult::SlowDown),
                    Err(error) => Err(OauthError::Provider(error.to_string())),
                }
            }
            OauthFlavor::Rfc8628 => {
                let client = Rfc8628Client {
                    issuer: pending.auth_server.clone(),
                    client_id: pending.client_id.clone(),
                    scopes: String::new(),
                };
                match client.poll_token_once(pending) {
                    Ok(tokens) => Ok(PollResult::Complete(tokens)),
                    Err(Rfc8628Error::AuthorizationPending) => Ok(PollResult::Pending),
                    Err(Rfc8628Error::SlowDown) => Ok(PollResult::SlowDown),
                    Err(error) => Err(OauthError::Provider(error.to_string())),
                }
            }
        }
    }

    pub(crate) fn refresh(
        profile: &ConnectProfile,
        refresh_token: &str,
    ) -> Result<OauthTokens, OauthError> {
        match flavor(profile)? {
            OauthFlavor::Chatgpt => ChatgptOauthClient::for_profile(profile)
                .refresh(refresh_token)
                .map_err(|error| OauthError::Provider(error.to_string())),
            OauthFlavor::Rfc8628 => rfc8628(profile)?
                .refresh_access_token(refresh_token)
                .map_err(|error| OauthError::Provider(error.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::OauthPending;

    fn profile(id: &str, auth: crate::AuthSpec) -> crate::ProviderSpec {
        crate::profile::test_spec(id, auth, vec![])
    }

    #[test]
    fn api_key_profiles_have_no_oauth_flavor() {
        let profile = profile("demo", crate::AuthSpec::api_key(crate::AuthPrompt::Never));
        assert!(flavor(&profile)
            .unwrap_err()
            .to_string()
            .contains("not OAuth"));
    }

    #[test]
    fn the_rfc8628_flow_reads_the_profile_auth_table() {
        let mut profile = crate::xai::xai_grok_profile();
        let client = rfc8628(&profile).unwrap();
        assert_eq!(client.issuer, crate::oauth_rfc8628::DEFAULT_ISSUER);
        assert_eq!(client.client_id, crate::oauth_rfc8628::DEFAULT_CLIENT_ID);
        assert!(client.scopes.contains("offline_access"));

        if let Some(auth) = profile.auth.as_mut() {
            auth.issuer = Some("https://issuer.example/".into());
        }
        assert_eq!(rfc8628(&profile).unwrap().issuer, "https://issuer.example");
    }

    /// `start` and `refresh` both resolve the flavor before making any
    /// request, so a non-OAuth profile is rejected without network access.
    /// These assert that short-circuit rather than the transport behind it.
    #[test]
    fn start_rejects_a_non_oauth_profile_without_contacting_a_server() {
        let profile = profile("demo", crate::AuthSpec::api_key(crate::AuthPrompt::Never));
        let err = OauthDispatcher::start(&profile).unwrap_err();
        assert!(
            matches!(err, OauthError::NotOauth(ref id) if id == "demo"),
            "expected NotOauth(\"demo\"), got {err:?}"
        );
        assert_eq!(err.to_string(), "profile `demo` is not OAuth");
    }

    #[test]
    fn refresh_rejects_a_non_oauth_profile_without_contacting_a_server() {
        let profile = profile("demo", crate::AuthSpec::api_key(crate::AuthPrompt::Never));
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

    #[test]
    fn codex_oauth_dispatch_start_poll_and_refresh_use_mock_server() {
        use forge_test_support::mock_http;
        let Some(base) = mock_http(vec![
            (
                200,
                r#"{"device_auth_id":"device-1","user_code":"AB12","interval":1}"#,
                vec![],
            ),
            (
                200,
                r#"{"authorization_code":"auth-code","code_verifier":"verifier"}"#,
                vec![],
            ),
            (
                200,
                r#"{"access_token":"access","refresh_token":"refresh","expires_in":3600}"#,
                vec![],
            ),
            (
                200,
                r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":120}"#,
                vec![],
            ),
        ]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };

        let mut profile = crate::openai_codex::openai_codex_profile();
        // The test-only issuer override: the flow talks to the mock.
        if let Some(auth) = profile.auth.as_mut() {
            auth.issuer = Some(base.clone());
        }
        let pending = OauthDispatcher::start(&profile).unwrap();
        assert_eq!(pending.profile_id, "openai_codex");
        assert_eq!(pending.device_code, "device-1");

        let poll = OauthDispatcher::poll(&pending).unwrap();
        assert!(matches!(poll, PollResult::Complete(_)));

        let tokens = OauthDispatcher::refresh(&profile, "old-refresh").unwrap();
        assert_eq!(tokens.access_token, "new-access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("new-refresh"));
    }

    #[test]
    fn codex_oauth_dispatch_poll_maps_pending_and_slowdown() {
        use forge_test_support::mock_http;
        let Some(base) = mock_http(vec![
            (403, "", vec![]),
            (400, r#"{"error":"slow_down"}"#, vec![]),
        ]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };

        let pending = OauthPending {
            profile_id: crate::openai_codex::PROFILE_ID.into(),
            verification_uri: format!("{base}/codex/device"),
            verification_uri_complete: None,
            user_code: "AB12".into(),
            device_code: "device-1".into(),
            auth_server: base.clone(),
            interval_secs: 1,
            expires_in_secs: Some(900),
            client_id: crate::oauth_chatgpt::CLIENT_ID.into(),
            flavor: crate::OauthFlavor::Chatgpt,
        };

        assert!(matches!(
            OauthDispatcher::poll(&pending).unwrap(),
            PollResult::Pending
        ));
        assert!(matches!(
            OauthDispatcher::poll(&pending).unwrap(),
            PollResult::SlowDown
        ));
    }

    #[test]
    fn xai_oauth_dispatch_start_poll_and_refresh_use_mock_server() {
        use forge_test_support::mock_http;
        let Some(base) = mock_http(vec![
            (
                200,
                r#"{"device_code":"dc","user_code":"AB12","verification_uri":"https://accounts.x.ai/oauth2/device","interval":1,"expires_in":900}"#,
                vec![],
            ),
            (
                200,
                r#"{"access_token":"access","refresh_token":"refresh","expires_in":3600}"#,
                vec![],
            ),
            (
                200,
                r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":120}"#,
                vec![],
            ),
        ]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };

        let mut profile = crate::xai::xai_grok_profile();
        // The test-only issuer override: the flow talks to the mock.
        if let Some(auth) = profile.auth.as_mut() {
            auth.issuer = Some(base.clone());
        }
        let pending = OauthDispatcher::start(&profile).unwrap();
        assert_eq!(pending.profile_id, "xai");
        assert_eq!(pending.device_code, "dc");

        let poll = OauthDispatcher::poll(&pending).unwrap();
        assert!(matches!(poll, PollResult::Complete(_)));

        let tokens = OauthDispatcher::refresh(&profile, "old-refresh").unwrap();
        assert_eq!(tokens.access_token, "new-access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("new-refresh"));
    }

    #[test]
    fn xai_oauth_dispatch_poll_maps_pending_and_slowdown() {
        use forge_test_support::mock_http;
        let Some(base) = mock_http(vec![
            (400, r#"{"error":"authorization_pending"}"#, vec![]),
            (400, r#"{"error":"slow_down"}"#, vec![]),
        ]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };

        let pending = OauthPending {
            profile_id: crate::xai::PROFILE_ID.into(),
            verification_uri: format!("{base}/oauth2/device"),
            verification_uri_complete: None,
            user_code: "AB12".into(),
            device_code: "dc".into(),
            auth_server: base,
            interval_secs: 1,
            expires_in_secs: Some(900),
            client_id: crate::oauth_rfc8628::DEFAULT_CLIENT_ID.into(),
            flavor: crate::OauthFlavor::Rfc8628,
        };

        assert!(matches!(
            OauthDispatcher::poll(&pending).unwrap(),
            PollResult::Pending
        ));
        assert!(matches!(
            OauthDispatcher::poll(&pending).unwrap(),
            PollResult::SlowDown
        ));
    }
}
