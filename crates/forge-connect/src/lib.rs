//! Phase 6 connect profiles and `/connect` support (CONN-01, PROV-01, PROV-02).

mod profile;
mod registry;
mod service;
mod store;
mod xai;

pub use profile::{ConnectOutcome, ConnectProfile, ConnectStatus, KeySource};
pub use registry::{builtin_registry, ConnectRegistry};
pub use service::{
    format_connected, handle_connect_action, parse_connect_args, ConnectAction, ConnectError,
    ConnectService,
};
pub use store::{resolve_key, CredentialStore, StoreError};
pub use xai::{xai_grok_profile, PROFILE_ID as XAI_PROFILE_ID};

/// Register built-in profiles.
pub(crate) fn register_builtin_profiles(registry: &mut ConnectRegistry) {
    registry.register(xai::xai_grok_profile());
    // PROV-02 OpenCode Go registered in follow-up commit.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_includes_xai() {
        let r = builtin_registry();
        assert!(r.get("xai").is_some());
        assert_eq!(r.get("xai").unwrap().title, "xAI Grok");
    }
}
