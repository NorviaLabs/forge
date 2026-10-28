//! OpenAI Codex subscription profile and token helpers.

use crate::oauth_chatgpt::{API_BASE_ENV, DEFAULT_BASE_URL};
use crate::profile::{
    Attach, AuthKind, AuthPrompt, AuthSpec, CatalogKind, CatalogSpec, EffortFamily, OauthFlavor,
    WireKind,
};
use crate::{AuthMode, CatalogMode, ProviderSpec, ProviderTransport, SpecOrigin};

pub const PROFILE_ID: &str = "openai_codex";
pub const AUTH_SERVER: &str = "https://auth.openai.com";

pub fn openai_codex_profile() -> ProviderSpec {
    ProviderSpec {
        id: PROFILE_ID.into(),
        title: "OpenAI Codex subscription".into(),
        description: "Use a ChatGPT plan through Forge's own device login".into(),
        auth_mode: AuthMode::Oauth {
            device_code: true,
            system_browser: true,
            auth_server: AUTH_SERVER.into(),
        },
        api_key_env: vec![],
        base_url: DEFAULT_BASE_URL.into(),
        default_models: vec!["openai-codex/gpt-5.6-sol".into()],
        // Entitlement still comes from the live Codex catalog; models.dev
        // only supplies public metadata such as effort and image input.
        models_dev_providers: vec!["openai".into()],
        auth_url: Some("https://auth.openai.com/codex/device".into()),
        model_provider_prefix: "openai-codex".into(),
        provider: "openai".into(),
        provider_label: "OpenAI".into(),
        label: "ChatGPT".into(),
        route_id: "openai-chatgpt".into(),
        catalog_mode: CatalogMode::LiveRegistry,
        transport: ProviderTransport::Codex,
        auth: Some(AuthSpec {
            kind: AuthKind::OauthDevice,
            flavor: Some(OauthFlavor::Chatgpt),
            client_id: Some(crate::oauth_chatgpt::CLIENT_ID.into()),
            prompt: AuthPrompt::Browser,
            ..AuthSpec::default()
        }),
        base_url_env: vec![API_BASE_ENV.into()],
        wire: Some(WireKind::Responses),
        attach: Some(Attach::BearerAccountId),
        catalog: CatalogSpec {
            kind: Some(CatalogKind::ChatgptModels),
            filter: None,
        },
        import_aliases: vec!["openai".into()],
        effort_family: EffortFamily::Openai,
        origin: SpecOrigin::Builtin,
        ..ProviderSpec::blank()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn profile_uses_forge_oauth() {
        let profile = openai_codex_profile();
        assert!(profile.auth_mode.is_oauth());
        assert!(profile.api_key_env.is_empty());
        assert!(profile
            .default_models
            .iter()
            .all(|model| model.starts_with("openai-codex/")));
    }

    #[test]
    fn extracts_chatgpt_account_from_access_token() {
        let payload = serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-123"
            }
        });
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string());
        assert_eq!(
            crate::oauth_chatgpt::account_id_from_token(&format!("header.{encoded}.signature"))
                .unwrap(),
            "account-123"
        );
    }
}
