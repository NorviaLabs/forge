//! Phase 6 / 6.1 connect profiles: OAuth (xAI) + API key (OpenCode Go).

mod auth;
mod opencode_go;
mod profile;
mod registry;
mod service;
mod store;
mod xai;

pub use auth::{AuthMode, OauthPending, OauthTokens};
pub use opencode_go::{opencode_go_profile, PROFILE_ID as OPENCODE_GO_PROFILE_ID};
pub use profile::{ConnectOutcome, ConnectProfile, ConnectStatus, KeySource};
pub use registry::{builtin_registry, ConnectRegistry};
pub use service::{
    format_connected, handle_connect_action, needs_tui_api_key_prompt, needs_tui_oauth,
    parse_connect_args, ConnectAction, ConnectError, ConnectService,
};
pub use store::{resolve_connected, resolve_key, CredentialStore, StoreError};
pub use xai::{xai_grok_profile, PROFILE_ID as XAI_PROFILE_ID};

pub(crate) fn register_builtin_profiles(registry: &mut ConnectRegistry) {
    registry.register(xai::xai_grok_profile());
    registry.register(opencode_go::opencode_go_profile());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_auth_modes() {
        let r = builtin_registry();
        assert!(r.get("xai").unwrap().auth_mode.is_oauth());
        assert!(r.get("opencode_go").unwrap().needs_tui_api_key_prompt());
        assert_eq!(r.profiles().len(), 2);
    }
}
