//! Provider-neutral reasoning effort selected for the current TUI session.

use std::{fmt, str::FromStr};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReasoningEffort {
    #[default]
    Auto,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl ReasoningEffort {
    pub const USAGE: &'static str = "auto|minimal|low|medium|high|xhigh|max";

    pub fn from_env() -> Self {
        Self::env_override().unwrap_or_default()
    }

    pub fn env_override() -> Option<Self> {
        std::env::var("FORGE_REASONING_EFFORT")
            .ok()
            .and_then(|value| value.parse().ok())
    }

    /// Empty means providers should choose their own default.
    pub fn transport_value(self) -> &'static str {
        match self {
            Self::Auto => "",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// Effort levels the selected model can actually consume.
    /// Keep in sync with `forge-model` native transports.
    pub fn options_for_model(model: &str) -> Vec<Self> {
        use ReasoningEffort::*;
        if !Self::model_supports_effort(model) {
            return vec![Auto];
        }
        let model = model.to_ascii_lowercase();
        if model.starts_with("openai-codex/") {
            return vec![Auto, Minimal, Low, Medium, High, XHigh, Max];
        }
        if model.starts_with("anthropic/") {
            let model_id = model.trim_start_matches("anthropic/");
            let mut options = vec![Auto, Low, Medium, High];
            // Transport remaps xhigh → high for these ids.
            if !(model_id.contains("4-6") || model_id.contains("opus-4-5")) {
                options.push(XHigh);
            }
            return options;
        }
        // OpenAI-compatible reasoning_effort path (OpenAI, OpenCode, xAI).
        vec![Auto, Minimal, Low, Medium, High, XHigh]
    }

    pub fn model_supports_effort(model: &str) -> bool {
        let model = model.to_ascii_lowercase();
        let model_id = model
            .split_once('/')
            .map(|(_, id)| id)
            .unwrap_or(model.as_str());

        if model.starts_with("openai-codex/") {
            return true;
        }
        // OpenCode Go/Zen use the OpenAI-compatible reasoning_effort field.
        if model.starts_with("opencode-") {
            return true;
        }
        if model.starts_with("openai/")
            && ["gpt-5", "o1", "o3", "o4"]
                .iter()
                .any(|prefix| model_id.starts_with(prefix))
        {
            return true;
        }
        if (model.starts_with("xai/") || model.starts_with("grok/"))
            && ["grok-4.3", "grok-4.5", "grok-4.20"]
                .iter()
                .any(|marker| model_id.contains(marker))
        {
            return true;
        }
        if model.starts_with("anthropic/")
            && [
                "sonnet-5",
                "opus-4-8",
                "opus-4-7",
                "opus-4-6",
                "sonnet-4-6",
                "opus-4-5",
            ]
            .iter()
            .any(|marker| model_id.contains(marker))
        {
            return true;
        }
        false
    }
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        })
    }
}

impl FromStr for ReasoningEffort {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" | "default" => Ok(Self::Auto),
            "minimal" | "min" => Ok(Self::Minimal),
            "low" | "light" => Ok(Self::Low),
            "medium" | "med" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" | "extra-high" | "extra_high" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_parse_to_canonical_values() {
        assert_eq!("light".parse(), Ok(ReasoningEffort::Low));
        assert_eq!("extra-high".parse(), Ok(ReasoningEffort::XHigh));
        assert_eq!(ReasoningEffort::XHigh.to_string(), "xhigh");
        assert_eq!(ReasoningEffort::Auto.transport_value(), "");
    }

    #[test]
    fn options_match_transport_support() {
        assert_eq!(
            ReasoningEffort::options_for_model("openai/gpt-4.1-mini"),
            vec![ReasoningEffort::Auto]
        );
        assert!(
            ReasoningEffort::options_for_model("openai/gpt-5.2").contains(&ReasoningEffort::High)
        );
        assert!(
            ReasoningEffort::options_for_model("opencode-go/claude-sonnet-4")
                .contains(&ReasoningEffort::Medium)
        );
        assert!(
            ReasoningEffort::options_for_model("openai-codex/gpt-5.6-sol")
                .contains(&ReasoningEffort::Max)
        );
        assert!(
            ReasoningEffort::options_for_model("anthropic/claude-sonnet-4-6")
                .contains(&ReasoningEffort::High)
        );
        assert!(
            !ReasoningEffort::options_for_model("anthropic/claude-sonnet-4-6")
                .contains(&ReasoningEffort::XHigh)
        );
    }
}
