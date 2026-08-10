//! Phase 6 / 6.1 connect profiles: OAuth (xAI) + API-key providers + Ollama.

mod anthropic;
mod auth;
mod catalog;
mod cost;
mod oauth_dispatch;
mod oauth_openai_codex;
mod oauth_xai;
mod ollama;
mod openai;
mod openai_codex;
mod opencode_go;
mod opencode_zen;
mod profile;
mod registry;
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
    credential_for_catalog, fetch_remote_models, group_routes, models_for_picker,
    normalize_model_id, refresh_models_dev_registry, refresh_profile_catalog, route_model_id,
    CatalogCost, CatalogEntry, CatalogError, CatalogSource, ModelCatalogCache, ModelPickerEntry,
    ModelRoute, DEFAULT_TTL_SECS, MODELS_DEV_TTL_SECS,
};
pub use cost::provider_cost_report;
pub use oauth_dispatch::OauthError;
pub use oauth_xai::{
    try_open_browser, XaiOauthClient, XaiOauthError, DEFAULT_CLIENT_ID, DEFAULT_ISSUER,
    DEFAULT_SCOPES,
};
pub use ollama::{
    ollama_profile, API_BASE_ENV as OLLAMA_API_BASE_ENV,
    DEFAULT_BASE_URL as OLLAMA_DEFAULT_BASE_URL, PROFILE_ID as OLLAMA_PROFILE_ID,
};
pub use openai::{openai_profile, PROFILE_ID as OPENAI_PROFILE_ID};
pub use openai_codex::{
    openai_codex_profile, ACCESS_TOKEN_ENV as OPENAI_CODEX_ACCESS_TOKEN_ENV,
    ACCOUNT_ID_ENV as OPENAI_CODEX_ACCOUNT_ID_ENV, PROFILE_ID as OPENAI_CODEX_PROFILE_ID,
};
pub use opencode_go::{
    opencode_go_profile, API_BASE_ENV as OPENCODE_API_BASE_ENV,
    DEFAULT_BASE_URL as OPENCODE_GO_DEFAULT_BASE_URL, PROFILE_ID as OPENCODE_GO_PROFILE_ID,
};
pub use opencode_zen::{
    opencode_zen_profile, API_BASE_ENV as OPENCODE_ZEN_API_BASE_ENV,
    DEFAULT_BASE_URL as OPENCODE_ZEN_DEFAULT_BASE_URL, PROFILE_ID as OPENCODE_ZEN_PROFILE_ID,
};
pub use profile::{ConnectOutcome, ConnectProfile, ConnectStatus, KeySource};
pub use registry::{builtin_registry, ConnectRegistry};
pub use selection::ModelSelection;
pub use service::{
    format_connected, handle_connect_action, needs_tui_api_key_prompt, needs_tui_oauth,
    parse_connect_args, ConnectAction, ConnectError, ConnectService,
};
pub use store::{
    credential_store_reads, resolve_connected, resolve_key, CredentialStore, StoreError,
};
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
        assert!(r
            .get("opencode_zen")
            .unwrap()
            .default_base_url
            .as_deref()
            .unwrap()
            .contains("/zen/v1"));
    }
}
