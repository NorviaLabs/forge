//! Context-pressure policy: when to compact, how much runway to buy, and
//! how much raw conversation to keep alongside the checkpoint.
//!
//! Pure arithmetic over model metadata — no I/O, no provider knowledge — so
//! the trigger boundary can be unit-tested without a session or a model.

use serde::{Deserialize, Serialize};

/// Fraction of the context window that is the hard upper bound for
/// utilization before compaction runs (§5).
pub const TRIGGER_UTILIZATION: f64 = 0.85;

/// Fraction of the context window compaction aims to land at or below (§16).
/// Reducing 85% to 75% would not buy enough runway to justify breaking the
/// cached prefix, so the candidate context is rejected above this.
pub const POST_COMPACTION_TARGET: f64 = 0.40;

/// Fraction of the window used as the raw-tail target (§10).
pub const TAIL_FRACTION: f64 = 0.12;
pub const TAIL_MIN_TOKENS: usize = 16_000;
pub const TAIL_MAX_TOKENS: usize = 64_000;

/// Default output-token reserve when model metadata does not advertise one.
/// Matches the `max_tokens` the Anthropic transport sends.
pub const DEFAULT_OUTPUT_RESERVE: usize = 8_192;

/// Headroom held back beyond the output reserve for the tokenizer estimate
/// being an estimate, provider-side prompt overhead, and the next tool result.
pub const DEFAULT_SAFETY_RESERVE: usize = 8_192;

/// Input tokens the next turn is assumed to append (one assistant step plus
/// one tool result). Used by the "will the *next* turn fit" half of the
/// trigger so compaction happens before the request that would overflow,
/// not after it fails.
pub const DEFAULT_EXPECTED_TURN_TOKENS: usize = 8_192;

/// Why compaction ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionTrigger {
    /// Context pressure crossed the policy boundary during `prepare_model_step`.
    Automatic,
    /// The operator ran `/compact`.
    Manual,
}

impl CompactionTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Manual => "manual",
        }
    }
}

/// Context-window arithmetic for one model.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CompactionPolicy {
    /// Total context window advertised for the active model.
    pub context_window: usize,
    /// Tokens reserved for the model's own output.
    pub max_output_reserve: usize,
    /// Extra headroom beyond the output reserve.
    pub safety_reserve: usize,
    /// Input tokens the next turn is assumed to add.
    pub expected_turn_tokens: usize,
}

impl CompactionPolicy {
    /// Policy for a model with `context_window` tokens, taking the provider's
    /// advertised output limit when known.
    pub fn for_window(context_window: usize, max_output: Option<usize>) -> Self {
        Self {
            context_window,
            max_output_reserve: max_output.unwrap_or(DEFAULT_OUTPUT_RESERVE),
            safety_reserve: DEFAULT_SAFETY_RESERVE,
            expected_turn_tokens: DEFAULT_EXPECTED_TURN_TOKENS,
        }
    }

    /// Window minus everything held back for output and safety.
    pub fn usable_context(&self) -> usize {
        self.context_window
            .saturating_sub(self.max_output_reserve)
            .saturating_sub(self.safety_reserve)
    }

    /// The pressure boundary: `min(window * 0.85, usable_context)`.
    pub fn trigger_threshold(&self) -> usize {
        let utilization_cap = (self.context_window as f64 * TRIGGER_UTILIZATION) as usize;
        utilization_cap.min(self.usable_context())
    }

    /// Compact when the current context plus the turn about to be appended
    /// would cross the boundary.
    pub fn should_compact(&self, current_context_tokens: usize) -> bool {
        current_context_tokens.saturating_add(self.expected_turn_tokens) >= self.trigger_threshold()
    }

    /// Upper bound a freshly compacted context must land at or below, so
    /// breaking the cached prefix actually buys runway.
    pub fn runway_limit(&self) -> usize {
        (self.context_window as f64 * POST_COMPACTION_TARGET) as usize
    }

    /// Target size for the retained raw tail:
    /// `clamp(window * 0.12, 16K, 64K)`, additionally capped at a third of
    /// the window so a small window cannot ask for a tail it cannot hold.
    pub fn tail_target_tokens(&self) -> usize {
        let scaled = (self.context_window as f64 * TAIL_FRACTION) as usize;
        scaled
            .clamp(TAIL_MIN_TOKENS, TAIL_MAX_TOKENS)
            .min(self.context_window / 3)
    }

    pub fn utilization(&self, tokens: usize) -> f64 {
        tokens as f64 / self.context_window.max(1) as f64
    }
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self::for_window(200_000, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_is_the_lower_of_the_utilization_cap_and_the_usable_window() {
        // 200K window: 0.85 * 200K = 170K; usable = 200K - 8192 - 8192 = 183_616.
        let policy = CompactionPolicy::for_window(200_000, None);
        assert_eq!(policy.usable_context(), 183_616);
        assert_eq!(policy.trigger_threshold(), 170_000);

        // A large output reserve makes the usable window the binding constraint.
        let policy = CompactionPolicy::for_window(200_000, Some(64_000));
        assert_eq!(policy.usable_context(), 127_808);
        assert_eq!(policy.trigger_threshold(), 127_808);
    }

    #[test]
    fn does_not_trigger_until_the_next_turn_would_cross_the_boundary() {
        let policy = CompactionPolicy::for_window(200_000, None);
        let threshold = policy.trigger_threshold();
        assert!(!policy.should_compact(threshold - policy.expected_turn_tokens - 1));
        assert!(policy.should_compact(threshold - policy.expected_turn_tokens));
        assert!(policy.should_compact(threshold));
    }

    #[test]
    fn tail_target_follows_the_documented_window_examples() {
        for (window, expected) in [
            (128_000_usize, 16_000_usize),
            (200_000, 24_000),
            (256_000, 30_720),
            (1_000_000, 64_000),
        ] {
            assert_eq!(
                CompactionPolicy::for_window(window, None).tail_target_tokens(),
                expected,
                "window {window}"
            );
        }
    }

    #[test]
    fn tail_target_never_exceeds_a_third_of_a_small_window() {
        let policy = CompactionPolicy::for_window(9_000, None);
        assert_eq!(policy.tail_target_tokens(), 3_000);
    }

    #[test]
    fn runway_limit_is_well_below_the_trigger() {
        let policy = CompactionPolicy::for_window(200_000, None);
        assert_eq!(policy.runway_limit(), 80_000);
        assert!(policy.runway_limit() < policy.trigger_threshold() / 2 + 1);
    }
}
