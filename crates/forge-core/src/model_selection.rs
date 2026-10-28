//! Request-facing model selection state.

/// The execution-facing model state that must move together between turns.
/// Provider credentials stay outside this type; this is only the state needed
/// to build a request and report the effective profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelExecutionSelection {
    pub model: String,
    /// The profile the model id names (`openai_codex`); empty before a
    /// model is picked.
    pub profile_id: String,
    pub reasoning_effort: Option<String>,
    pub thinking_enabled: bool,
}

impl Default for ModelExecutionSelection {
    fn default() -> Self {
        Self {
            model: String::new(),
            profile_id: String::new(),
            reasoning_effort: None,
            thinking_enabled: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ModelExecutionSelection;

    #[test]
    fn default_selection_is_safe_and_unset() {
        assert_eq!(
            ModelExecutionSelection::default(),
            ModelExecutionSelection {
                model: String::new(),
                profile_id: String::new(),
                reasoning_effort: None,
                thinking_enabled: true,
            }
        );
    }
}
