use crate::auth::{OauthPending, OauthTokens};
use crate::oauth_openai_codex::{OpenAiCodexOauthClient, OpenAiCodexOauthError};
use crate::oauth_xai::{XaiOauthClient, XaiOauthError};
use crate::profile::ConnectProfile;

pub(crate) enum PollResult {
    Pending,
    SlowDown,
    Complete(OauthTokens),
}

pub(crate) struct OauthDispatcher;

impl OauthDispatcher {
    pub(crate) fn start(profile: &ConnectProfile) -> Result<OauthPending, String> {
        if profile.id == crate::openai_codex::PROFILE_ID {
            return OpenAiCodexOauthClient::start_device_code().map_err(|error| error.to_string());
        }

        Self::xai_client(profile)?
            .start_device_code(&profile.id)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn poll(pending: &OauthPending) -> Result<PollResult, String> {
        if pending.profile_id == crate::openai_codex::PROFILE_ID {
            return match OpenAiCodexOauthClient::poll_token_once(pending) {
                Ok(tokens) => Ok(PollResult::Complete(tokens)),
                Err(OpenAiCodexOauthError::Pending) => Ok(PollResult::Pending),
                Err(OpenAiCodexOauthError::SlowDown) => Ok(PollResult::SlowDown),
                Err(error) => Err(error.to_string()),
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
            Err(error) => Err(error.to_string()),
        }
    }

    pub(crate) fn refresh(
        profile: &ConnectProfile,
        refresh_token: &str,
    ) -> Result<OauthTokens, String> {
        if profile.id == crate::openai_codex::PROFILE_ID {
            return OpenAiCodexOauthClient::refresh(refresh_token)
                .map_err(|error| error.to_string());
        }

        Self::xai_client(profile)?
            .refresh_access_token(refresh_token)
            .map_err(|error| error.to_string())
    }

    fn xai_client(profile: &ConnectProfile) -> Result<XaiOauthClient, String> {
        let crate::auth::AuthMode::Oauth { auth_server, .. } = &profile.auth_mode else {
            return Err(format!("profile `{}` is not OAuth", profile.id));
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
