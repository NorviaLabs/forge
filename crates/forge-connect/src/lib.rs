//! Phase 6 connect profiles and `/connect` support (CONN-01).

mod profile;
mod registry;
mod service;
mod store;

pub use profile::{ConnectOutcome, ConnectProfile, ConnectStatus, KeySource};
pub use registry::{builtin_registry, ConnectRegistry};
pub use service::{
    format_connected, handle_connect_action, parse_connect_args, ConnectAction, ConnectError,
    ConnectService,
};
pub use store::{resolve_key, CredentialStore, StoreError};

/// Register built-in profiles. Provider modules fill this in Phase 6 commits.
pub(crate) fn register_builtin_profiles(_registry: &mut ConnectRegistry) {
    // PROV-01 / PROV-02 modules call register from here once added.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_starts_empty_until_providers_registered() {
        // After full Phase 6, this will include xai + opencode_go.
        let r = builtin_registry();
        let _ = r.profiles().len();
    }
}
