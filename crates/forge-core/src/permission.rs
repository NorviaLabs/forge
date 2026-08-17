//! Deriving the permission mode from what the host can actually enforce.
//!
//! The invariant this file exists to hold:
//!
//! > Auto is only ever entered when there is an enforcement floor underneath
//! > it.
//!
//! `AcceptEdits` ("Auto") frees writes. That is only defensible when a sandbox
//! confines what a freed write can reach. On a host that cannot confine, the
//! honest mode is `Manual` — forge asks rather than running agent commands
//! unconfined and silent.
//!
//! This is a deliberate divergence from Claude Code, whose default is to warn
//! and continue *unsandboxed*, hardened only by opting into
//! `sandbox.failIfUnavailable`. That trades the floor for convenience at
//! exactly the moment the user cannot see it.
//!
//! This lives in `forge-core` because it is the lowest layer that can see both
//! halves: `forge-governance` owns [`PermissionMode`] but depends on
//! `forge-types` alone, and `forge-tools` knows about sandboxes but nothing
//! about approval. A future headless entry point gets the derivation for free
//! by living above this crate.

use forge_governance::PermissionMode;

/// The most permissive mode this host may use, and why it is capped when it is.
///
/// The result is a **ceiling, not an override**. Narrowing is always safe, so a
/// user who prefers `Manual` keeps it; widening past the ceiling is what must
/// not happen. Callers clamp with [`PermissionMode::clamped_to`].
pub fn permission_ceiling() -> (PermissionMode, Option<String>) {
    match forge_tools::sandbox::availability() {
        Ok(()) => (PermissionMode::AcceptEdits, None),
        Err(unavailable) => (PermissionMode::Manual, Some(unavailable.reason())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceiling_matches_whether_this_host_can_confine() {
        let (ceiling, reason) = permission_ceiling();
        match forge_tools::sandbox::availability() {
            Ok(()) => {
                assert_eq!(ceiling, PermissionMode::AcceptEdits);
                assert!(reason.is_none(), "no cap means nothing to explain");
            }
            Err(_) => {
                assert_eq!(ceiling, PermissionMode::Manual);
                let reason = reason.expect("a capped mode must say why");
                assert!(!reason.is_empty());
            }
        }
    }

    /// The invariant, stated as a test: there is no combination of inputs that
    /// yields Auto without a sandbox.
    #[test]
    fn auto_is_unreachable_without_a_floor() {
        let capped = PermissionMode::AcceptEdits.clamped_to(PermissionMode::Manual);
        assert_eq!(capped, PermissionMode::Manual);
    }

    /// A user who wants to be asked more often is never overridden.
    #[test]
    fn narrowing_is_always_permitted() {
        assert_eq!(
            PermissionMode::Manual.clamped_to(PermissionMode::AcceptEdits),
            PermissionMode::Manual
        );
        assert_eq!(
            PermissionMode::Manual.clamped_to(PermissionMode::Manual),
            PermissionMode::Manual
        );
    }

    #[test]
    fn a_permitted_mode_passes_through_unchanged() {
        assert_eq!(
            PermissionMode::AcceptEdits.clamped_to(PermissionMode::AcceptEdits),
            PermissionMode::AcceptEdits
        );
    }
}
