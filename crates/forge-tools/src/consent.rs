//! Where a session keeps the consents its tools asked for.
//!
//! A tool that must not act without the user's say-so (screen control, a
//! form submit) checks [`SessionConsents::check`] and, when it has no grant,
//! returns [`crate::ToolError::ConsentRequired`]. The session raises that as
//! an approval; approving it records a grant here and re-runs the call.
//!
//! One store per session. A child agent gets a fresh, empty store: a
//! parent's consent is never inherited.

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex};

pub use forge_types::{ConsentFact, ConsentKind, ConsentRequest};

#[derive(Debug, Default)]
struct Grants {
    /// `(kind, scope)` granted until revoked.
    session: BTreeSet<(ConsentKind, String)>,
    /// `(kind, scope, call id)` granted for one run of one call.
    once: HashSet<(ConsentKind, String, String)>,
}

/// The consents granted in one session.
#[derive(Debug, Default)]
pub struct SessionConsents {
    grants: Mutex<Grants>,
}

impl SessionConsents {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Grants> {
        self.grants
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether `(kind, scope)` is granted for the session.
    pub fn is_granted(&self, kind: ConsentKind, scope: &str) -> bool {
        self.lock().session.contains(&(kind, scope.to_string()))
    }

    /// Grant `(kind, scope)` until the session ends or it is revoked.
    pub fn grant(&self, kind: ConsentKind, scope: &str) {
        self.lock().session.insert((kind, scope.to_string()));
    }

    /// Grant `(kind, scope)` for the next run of the call `call_id` only.
    pub fn grant_once(&self, kind: ConsentKind, scope: &str, call_id: &str) {
        self.lock()
            .once
            .insert((kind, scope.to_string(), call_id.to_string()));
    }

    /// Whether the call `call_id` may go ahead: a session grant, or a
    /// single-use grant for this very call, which this consumes.
    pub fn check(&self, kind: ConsentKind, scope: &str, call_id: Option<&str>) -> bool {
        let mut grants = self.lock();
        if grants.session.contains(&(kind, scope.to_string())) {
            return true;
        }
        match call_id {
            Some(call_id) => grants
                .once
                .remove(&(kind, scope.to_string(), call_id.to_string())),
            None => false,
        }
    }

    /// Drop every grant of `kind`, session and single-use alike.
    pub fn revoke(&self, kind: ConsentKind) {
        let mut grants = self.lock();
        grants.session.retain(|(granted, _)| *granted != kind);
        grants.once.retain(|(granted, _, _)| *granted != kind);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_granted_at_first() {
        let consents = SessionConsents::new();
        assert!(!consents.is_granted(ConsentKind::ScreenControl, "screen"));
        assert!(!consents.check(ConsentKind::FormSubmit, "example.com", Some("c1")));
    }

    #[test]
    fn a_session_grant_covers_its_scope_only() {
        let consents = SessionConsents::new();
        consents.grant(ConsentKind::FormSubmit, "localhost:5173");
        assert!(consents.check(ConsentKind::FormSubmit, "localhost:5173", Some("a")));
        assert!(consents.check(ConsentKind::FormSubmit, "localhost:5173", None));
        assert!(!consents.check(ConsentKind::FormSubmit, "example.com", Some("a")));
        assert!(!consents.check(ConsentKind::ScreenControl, "localhost:5173", Some("a")));
    }

    #[test]
    fn a_single_use_grant_is_for_one_call_and_is_consumed() {
        let consents = SessionConsents::new();
        consents.grant_once(ConsentKind::FormSubmit, "example.com", "call-1");
        assert!(!consents.check(ConsentKind::FormSubmit, "example.com", Some("call-2")));
        assert!(!consents.check(ConsentKind::FormSubmit, "example.com", None));
        assert!(consents.check(ConsentKind::FormSubmit, "example.com", Some("call-1")));
        assert!(
            !consents.check(ConsentKind::FormSubmit, "example.com", Some("call-1")),
            "a single-use grant is spent by the run it was for"
        );
        assert!(!consents.is_granted(ConsentKind::FormSubmit, "example.com"));
    }

    #[test]
    fn revoking_drops_every_grant_of_that_kind() {
        let consents = SessionConsents::new();
        consents.grant(ConsentKind::ScreenControl, "screen");
        consents.grant(ConsentKind::FormSubmit, "example.com");
        consents.grant_once(ConsentKind::ScreenControl, "screen", "c");
        consents.revoke(ConsentKind::ScreenControl);
        assert!(!consents.is_granted(ConsentKind::ScreenControl, "screen"));
        assert!(!consents.check(ConsentKind::ScreenControl, "screen", Some("c")));
        assert!(consents.is_granted(ConsentKind::FormSubmit, "example.com"));
    }
}
