//! The per-process credentials handle (design §3).
//!
//! One `Arc<Credentials>` owns the credential store, the session-key overlay
//! (keys typed in the TUI, held for this process only), and per-profile
//! refresh single-flight. Every write, clear, session key, and auth report
//! goes through [`Credentials::slot_for`], so a `credential_from` profile
//! shares its target's slot.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use crate::auth::OauthTokens;
use crate::profile::ProviderSpec;
use crate::service::ConnectError;
use crate::store::{CredentialStore, StoreError};

pub struct Credentials {
    store: CredentialStore,
    /// slot → key typed this process. Never written to disk.
    session_keys: Mutex<BTreeMap<String, String>>,
    /// profile id → refresh guard, so concurrent requests refresh once.
    refresh_guards: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("store", &self.store.path())
            .finish_non_exhaustive()
    }
}

impl Credentials {
    pub fn new(store: CredentialStore) -> Self {
        Self {
            store,
            session_keys: Mutex::new(BTreeMap::new()),
            refresh_guards: Mutex::new(HashMap::new()),
        }
    }

    pub fn user_default() -> Self {
        Self::new(CredentialStore::user_default())
    }

    /// The process's handle over the user credential store. Every caller
    /// that does not hold an explicit handle (the CLI, session open, the
    /// TUI's default) shares this one, so session keys and refresh
    /// single-flight are per process.
    pub fn shared() -> Arc<Self> {
        static SHARED: std::sync::OnceLock<Arc<Credentials>> = std::sync::OnceLock::new();
        Arc::clone(SHARED.get_or_init(|| Arc::new(Self::user_default())))
    }

    /// A handle over the same file as `store`, for tests that hold a store.
    #[cfg(test)]
    pub(crate) fn for_test(store: &CredentialStore) -> Self {
        Self::new(CredentialStore::new(store.path().to_path_buf()))
    }

    pub fn store(&self) -> &CredentialStore {
        &self.store
    }

    /// The slot a profile reads and writes: its `credential_from` target,
    /// else its own id.
    pub fn slot_for(profile: &ProviderSpec) -> &str {
        profile.credential_from.as_deref().unwrap_or(&profile.id)
    }

    /// Hold a key typed in the TUI for this process. Callers set it only
    /// after a connect probe that did not return 401 or 403.
    pub fn set_session_key(&self, profile: &ProviderSpec, key: &str) {
        let key = key.trim();
        if key.is_empty() {
            return;
        }
        self.session_keys()
            .insert(Self::slot_for(profile).into(), key.into());
    }

    pub(crate) fn session_key(&self, profile: &ProviderSpec) -> Option<String> {
        self.session_keys()
            .get(Self::slot_for(profile))
            .filter(|key| !key.trim().is_empty())
            .cloned()
    }

    /// Store a key in the profile's slot. Clears its `auth_required` flag.
    pub fn set_api_key(&self, profile: &ProviderSpec, key: &str) -> Result<(), StoreError> {
        self.store.set_api_key(Self::slot_for(profile), key)
    }

    pub fn set_oauth(&self, profile: &ProviderSpec, tokens: OauthTokens) -> Result<(), StoreError> {
        self.store.set_oauth(Self::slot_for(profile), tokens)
    }

    /// Whether the profile's stored credential is flagged `auth_required`.
    pub fn is_flagged(&self, profile: &ProviderSpec) -> Result<bool, StoreError> {
        self.store.needs_authentication(Self::slot_for(profile))
    }

    /// Remove the profile's stored credential: its slot, its own old slot,
    /// and their session keys. Returns whether anything was removed.
    pub fn clear(&self, profile: &ProviderSpec) -> Result<bool, StoreError> {
        let slot = Self::slot_for(profile).to_string();
        let mut removed = self.session_keys().remove(&slot).is_some();
        removed |= self.store.clear(&slot)?;
        if slot != profile.id {
            removed |= self.session_keys().remove(&profile.id).is_some();
            removed |= self.store.clear(&profile.id)?;
        }
        Ok(removed)
    }

    /// Remove every stored credential and every session key.
    pub fn clear_all(&self) -> Result<bool, StoreError> {
        let mut keys = self.session_keys();
        let had_session = !keys.is_empty();
        keys.clear();
        drop(keys);
        Ok(self.store.clear_all()? || had_session)
    }

    /// Flag the profile's stored credential and drop its session key.
    pub(crate) fn report_auth_failure(&self, profile: &ProviderSpec) -> Result<(), StoreError> {
        self.session_keys().remove(Self::slot_for(profile));
        self.store.mark_auth_required(Self::slot_for(profile), true)
    }

    pub(crate) fn report_auth_success(&self, profile: &ProviderSpec) -> Result<(), StoreError> {
        self.store
            .mark_auth_required(Self::slot_for(profile), false)
    }

    /// Refresh the profile's OAuth token when it expires within five
    /// minutes. Blocking (network); single-flight per profile, so a second
    /// caller waits and then sees the fresh token. A no-op for key profiles.
    pub fn ensure_fresh_blocking(&self, profile: &ProviderSpec) -> Result<(), ConnectError> {
        if !profile.effective_auth().is_oauth() {
            return Ok(());
        }
        let guard = self
            .refresh_guards
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(profile.id.clone())
            .or_default()
            .clone();
        let _flight = guard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        crate::service::fresh_oauth_tokens(profile, &self.store).map(|_| ())
    }

    fn session_keys(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, String>> {
        self.session_keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn credentials(dir: &tempfile::TempDir) -> Credentials {
        Credentials::new(CredentialStore::new(dir.path().join("credentials.toml")))
    }

    #[test]
    fn a_zen_write_lands_in_the_go_slot_and_disconnect_clears_both() {
        let dir = tempdir().unwrap();
        let creds = credentials(&dir);
        let zen = crate::opencode_zen::opencode_zen_profile();
        // An upgrade left a Zen-only key in the old slot (its read is
        // covered in `resolve`).
        creds
            .store()
            .set_api_key("opencode_zen", "old-zen")
            .unwrap();

        creds.set_api_key(&zen, "shared").unwrap();
        assert_eq!(
            creds.store().get_api_key("opencode_go").unwrap().as_deref(),
            Some("shared")
        );

        assert!(creds.clear(&zen).unwrap());
        assert_eq!(creds.store().get_api_key("opencode_go").unwrap(), None);
        assert_eq!(creds.store().get_api_key("opencode_zen").unwrap(), None);
    }

    #[test]
    fn clearing_or_flagging_a_slot_drops_its_session_key() {
        let dir = tempdir().unwrap();
        let creds = credentials(&dir);
        let openai = crate::openai::openai_profile();
        creds.set_session_key(&openai, "typed");
        assert_eq!(creds.session_key(&openai).as_deref(), Some("typed"));
        creds.report_auth_failure(&openai).unwrap();
        assert_eq!(creds.session_key(&openai), None);

        creds.set_session_key(&openai, "typed");
        creds.clear(&openai).unwrap();
        assert_eq!(creds.session_key(&openai), None);

        creds.set_session_key(&openai, "typed");
        assert!(creds.clear_all().unwrap());
        assert_eq!(creds.session_key(&openai), None);
    }

    #[test]
    fn concurrent_refreshes_of_a_profile_run_one_at_a_time() {
        let dir = tempdir().unwrap();
        let creds = Arc::new(credentials(&dir));
        let xai = crate::xai::xai_grok_profile();
        let guard = creds
            .refresh_guards
            .lock()
            .unwrap()
            .entry(xai.id.clone())
            .or_default()
            .clone();
        // While one refresh holds the guard, a second caller waits.
        let held = guard.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = {
            let creds = Arc::clone(&creds);
            let xai = xai.clone();
            std::thread::spawn(move || {
                creds.ensure_fresh_blocking(&xai).unwrap();
                tx.send(()).unwrap();
            })
        };
        assert!(rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err());
        drop(held);
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        worker.join().unwrap();
    }
}
