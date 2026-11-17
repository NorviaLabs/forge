//! The words that explain a model choice — a subagent's model, an Auto
//! turn's — in one place, so every reason names things the same way.

use super::auto::SUCCESSES_TO_STEP_DOWN;

/// `model @ effort`, the model's default effort as `default`.
pub fn model_at_effort(model: &str, effort: Option<&str>) -> String {
    format!("{model} @ {}", effort.unwrap_or("default"))
}

/// Why an Auto turn starts on the fallback when the session has no ranking
/// to ask.
pub(crate) const NO_RANKING: &str = "no ranking of models is available to this session";

/// Why an Auto turn runs `step` steps above `start` (of `top`): a failure.
pub(crate) fn stepped_up(step: usize, top: usize, start: &str) -> String {
    format!("step {step} of {top} above the start ({start}), after a failed turn")
}

/// Why an Auto turn runs `step` steps above `start` (of `top`), lower than
/// before: successes.
pub(crate) fn stepped_down(step: usize, top: usize, start: &str) -> String {
    format!(
        "step {step} of {top} above the start ({start}), lowered after \
         {SUCCESSES_TO_STEP_DOWN} successful turns"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_is_named_with_its_effort() {
        assert_eq!(
            model_at_effort("anthropic/claude-opus-4-1", Some("high")),
            "anthropic/claude-opus-4-1 @ high"
        );
        assert_eq!(model_at_effort("xai/grok-4", None), "xai/grok-4 @ default");
    }

    #[test]
    fn autos_reasons_read_as_before() {
        assert_eq!(
            NO_RANKING,
            "no ranking of models is available to this session"
        );
        assert_eq!(
            stepped_up(2, 3, "test/mid @ medium"),
            "step 2 of 3 above the start (test/mid @ medium), after a failed turn"
        );
        assert_eq!(
            stepped_down(1, 3, "test/mid @ medium"),
            "step 1 of 3 above the start (test/mid @ medium), lowered after 2 successful turns"
        );
    }
}
