//! Phase 6 / 6.1 connect profiles: OAuth (xAI) + API-key providers + Ollama.

mod anthropic;
mod atomic;
mod auth;
mod catalog;
mod cost;
mod credentials;
pub mod effort;
mod import;
mod normalize;
mod oauth_chatgpt;
mod oauth_dispatch;
mod oauth_rfc8628;
mod ollama;
mod openai;
mod openai_codex;
mod opencode_go;
mod opencode_zen;
mod preferences;
mod profile;
mod registry;
mod resolve;
mod route;
mod selection;
mod service;
mod store;
#[cfg(test)]
mod test_env;
#[cfg(test)]
mod test_support;
mod verify;
mod xai;

pub use anthropic::{anthropic_profile, PROFILE_ID as ANTHROPIC_PROFILE_ID};
pub use auth::{AuthMode, OauthPending, OauthTokens};
pub use catalog::{
    fetch_remote_models, models_for_picker, refresh_models_dev_registry, refresh_profile_catalog,
    runnable_models_for_picker, CatalogCost, CatalogEntry, CatalogError, CatalogLimits,
    CatalogSource, ModelCatalogCache, DEFAULT_TTL_SECS, MODELS_DEV_TTL_SECS,
};
pub use cost::provider_cost_report;
pub use credentials::Credentials;
pub use import::{
    discover_logins, discover_logins_in, login_for_profile, xdg_data_home, DiscoveredLogin,
    DiscoveredSecret,
};
pub use oauth_dispatch::OauthError;
pub use oauth_rfc8628::{
    try_open_browser, Rfc8628Client, Rfc8628Error, DEFAULT_CLIENT_ID, DEFAULT_ISSUER,
    DEFAULT_SCOPES,
};
pub use ollama::{
    ollama_profile, API_BASE_ENV as OLLAMA_API_BASE_ENV,
    DEFAULT_BASE_URL as OLLAMA_DEFAULT_BASE_URL, PROFILE_ID as OLLAMA_PROFILE_ID,
};
pub use openai::{openai_profile, PROFILE_ID as OPENAI_PROFILE_ID};
pub use openai_codex::{openai_codex_profile, PROFILE_ID as OPENAI_CODEX_PROFILE_ID};
pub use opencode_go::{
    opencode_go_profile, API_BASE_ENV as OPENCODE_API_BASE_ENV,
    DEFAULT_BASE_URL as OPENCODE_GO_DEFAULT_BASE_URL, PROFILE_ID as OPENCODE_GO_PROFILE_ID,
};
pub use opencode_zen::{
    opencode_zen_profile, API_BASE_ENV as OPENCODE_ZEN_API_BASE_ENV,
    DEFAULT_BASE_URL as OPENCODE_ZEN_DEFAULT_BASE_URL, PROFILE_ID as OPENCODE_ZEN_PROFILE_ID,
};
pub use preferences::PreferenceStore;
pub use profile::{
    Attach, AuthKind, AuthPrompt, AuthSpec, CatalogFilter, CatalogKind, CatalogMode, CatalogSpec,
    ConnectOutcome, ConnectProfile, ConnectStatus, EditableField, EffortFamily, KeySource,
    OauthFlavor, ProviderSpec, ProviderTransport, SessionHeaderKind, SpecOrigin, StaticHeader,
    VerifyKind, VerifySpec, WireKind, WireOverride,
};
pub use registry::{
    builtin_registry, credential_env_names, loaded_registry, loaded_registry_from,
    save_custom_provider, save_manual_model, update_custom_provider, ConnectRegistry,
};
pub use resolve::{
    env_names_for, resolve, winning_env_var, Credential, CredentialError, CredentialValue,
    ResolvedProfile, RouteDefinition, RouteTable,
};
pub use route::{
    group_routes, normalize_model_id, route_id_for_profile, route_model_id, ModelPickerEntry,
    ModelRoute,
};
pub use selection::ModelSelection;
pub use service::{
    format_connected, handle_connect_action, has_connected_profile, needs_tui_api_key_prompt,
    needs_tui_oauth, parse_connect_args, ConnectAction, ConnectError, ConnectService,
};
pub use store::{CredentialStore, StoreError};
pub use verify::VerifyError;
pub use xai::{xai_grok_profile, PROFILE_ID as XAI_PROFILE_ID};

pub(crate) fn register_builtin_profiles(registry: &mut ConnectRegistry) {
    registry.register(xai::xai_grok_profile());
    registry.register(opencode_go::opencode_go_profile());
    registry.register(opencode_zen::opencode_zen_profile());
    registry.register(openai::openai_profile());
    registry.register(openai_codex::openai_codex_profile());
    registry.register(anthropic::anthropic_profile());
    registry.register(ollama::ollama_profile());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The §1.1 fields each built-in fills (design §1.2 and the env table).
    #[test]
    fn builtin_schema_fields() {
        let r = builtin_registry();
        let shape = |id: &str| {
            let p = r.get(id).unwrap();
            let auth = p.effective_auth();
            (
                p.effective_wire(),
                auth.kind,
                auth.flavor,
                p.attach,
                p.catalog.kind,
                p.base_url_env.clone(),
                p.effort_family,
            )
        };
        let env = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(
            shape("openai"),
            (
                WireKind::ChatCompletions,
                AuthKind::ApiKey,
                None,
                Some(Attach::Bearer),
                Some(CatalogKind::OpenaiModels),
                env(&["OPENAI_API_BASE"]),
                EffortFamily::Openai,
            )
        );
        assert_eq!(
            shape("openai_codex"),
            (
                WireKind::Responses,
                AuthKind::OauthDevice,
                Some(OauthFlavor::Chatgpt),
                Some(Attach::BearerAccountId),
                Some(CatalogKind::ChatgptModels),
                env(&["FORGE_CODEX_API_BASE"]),
                EffortFamily::Openai,
            )
        );
        assert_eq!(
            shape("anthropic"),
            (
                WireKind::AnthropicMessages,
                AuthKind::ApiKey,
                None,
                Some(Attach::XApiKey),
                Some(CatalogKind::AnthropicModels),
                env(&["ANTHROPIC_API_BASE"]),
                EffortFamily::None,
            )
        );
        assert_eq!(
            shape("xai"),
            (
                WireKind::ChatCompletions,
                AuthKind::OauthDevice,
                Some(OauthFlavor::Rfc8628),
                Some(Attach::Bearer),
                Some(CatalogKind::OpenaiModels),
                env(&["XAI_API_BASE"]),
                EffortFamily::Xai,
            )
        );
        assert_eq!(
            shape("ollama"),
            (
                WireKind::ChatCompletions,
                AuthKind::ApiKeyOptional,
                None,
                Some(Attach::Bearer),
                Some(CatalogKind::OllamaTags),
                env(&["OLLAMA_API_BASE"]),
                EffortFamily::None,
            )
        );
        assert_eq!(
            shape("opencode_go").5,
            env(&["OPENCODE_API_BASE", "OPENCODE_GO_API_BASE"])
        );
        assert_eq!(shape("opencode_zen").5, env(&["OPENCODE_ZEN_API_BASE"]));
        assert_eq!(
            r.get("ollama").unwrap().editable,
            vec![EditableField::BaseUrl]
        );
        assert_eq!(
            r.get("opencode_zen").unwrap().credential_from.as_deref(),
            Some("opencode_go")
        );
        for id in ["opencode_go", "opencode_zen"] {
            assert_eq!(
                r.get(id).unwrap().session_header,
                Some(SessionHeaderKind::Opencode)
            );
        }
        // Today's `auth_mode` still agrees with the new `auth` table.
        for p in r.profiles() {
            assert_eq!(p.effective_auth().to_auth_mode(), p.auth_mode, "{}", p.id);
        }
    }

    #[test]
    fn builtin_auth_modes() {
        let r = builtin_registry();
        assert!(r.get("xai").unwrap().auth_mode.is_oauth());
        assert!(r.get("opencode_go").unwrap().needs_tui_api_key_prompt());
        assert!(r.get("opencode_zen").unwrap().needs_tui_api_key_prompt());
        assert!(r.get("openai").unwrap().needs_tui_api_key_prompt());
        assert!(!r.get("openai_codex").unwrap().needs_tui_api_key_prompt());
        assert!(r.get("anthropic").unwrap().needs_tui_api_key_prompt());
        assert!(!r.get("ollama").unwrap().needs_tui_api_key_prompt());
        assert_eq!(r.profiles().len(), 7);
        assert!(r.get("opencode_zen").unwrap().base_url.contains("/zen/v1"));
    }
}
