//! Pure launch decisions for first-install / new-project / returning.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchDecision {
    pub run_theme_setup: bool,
    pub run_trust_setup: bool,
    /// Resume picker only when this directory was already trusted at process start.
    pub allow_resume_picker: bool,
    pub require_connect: bool,
    pub show_ready_placeholder: bool,
}

pub fn decide_launch(
    theme_committed: bool,
    trusted_at_start: bool,
    provider_connected: bool,
) -> LaunchDecision {
    LaunchDecision {
        run_theme_setup: !theme_committed,
        run_trust_setup: !trusted_at_start,
        allow_resume_picker: theme_committed && trusted_at_start,
        require_connect: !provider_connected,
        show_ready_placeholder: !theme_committed || !trusted_at_start || !provider_connected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_install_runs_theme_then_trust() {
        let d = decide_launch(false, false, false);
        assert!(d.run_theme_setup);
        assert!(d.run_trust_setup);
        assert!(!d.allow_resume_picker);
        assert!(d.require_connect);
        assert!(d.show_ready_placeholder);
    }

    #[test]
    fn new_project_skips_theme() {
        let d = decide_launch(true, false, true);
        assert!(!d.run_theme_setup);
        assert!(d.run_trust_setup);
        assert!(!d.allow_resume_picker);
        assert!(!d.require_connect);
        assert!(d.show_ready_placeholder);
    }

    #[test]
    fn returning_opens_normal_tui() {
        let d = decide_launch(true, true, true);
        assert!(!d.run_theme_setup);
        assert!(!d.run_trust_setup);
        assert!(d.allow_resume_picker);
        assert!(!d.require_connect);
        assert!(!d.show_ready_placeholder);
    }

    #[test]
    fn inherited_trust_is_returning() {
        let d = decide_launch(true, true, true);
        assert!(d.allow_resume_picker);
        assert!(!d.show_ready_placeholder);
    }

    #[test]
    fn cancelled_connect_resumes_at_provider() {
        let d = decide_launch(true, true, false);
        assert!(!d.run_theme_setup);
        assert!(!d.run_trust_setup);
        assert!(d.allow_resume_picker);
        assert!(d.require_connect);
        assert!(d.show_ready_placeholder);
    }
}
