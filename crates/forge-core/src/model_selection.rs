//! Request-facing model selection state.

/// The execution-facing model state that must move together between turns.
/// Provider credentials stay outside this type; this is only the state needed
/// to build a request and report the effective route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelExecutionSelection {
    pub model: String,
    pub route_id: String,
    pub reasoning_effort: Option<String>,
    pub thinking_enabled: bool,
}

impl Default for ModelExecutionSelection {
    fn default() -> Self {
        Self {
            model: String::new(),
            route_id: String::new(),
            reasoning_effort: None,
            thinking_enabled: true,
        }
    }
}
