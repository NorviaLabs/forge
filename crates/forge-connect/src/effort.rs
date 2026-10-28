//! Reasoning-effort support per model (design §1.2, stage 5).
//!
//! One rule set for the picker and every transport. First match wins, on the
//! model's selected wire:
//!
//! 1. The models.dev catalog's effort options for the model, when present.
//! 2. The ChatGPT OAuth flavor: every level, including `Max`.
//! 3. The OpenCode session header: every level up to `XHigh`.
//! 4. The `anthropic-messages` wire: Claude models that take effort, with
//!    `XHigh` only where the model accepts it, default `Low`.
//! 5. `effort_family`: OpenAI reasoning models, or xAI Grok 4.x.
//! 6. Otherwise no adjustable effort.

use crate::catalog::ModelCatalogCache;
use crate::profile::{EffortFamily, OauthFlavor, SessionHeaderKind, WireKind};
use crate::resolve::RouteDefinition;

/// One reasoning-effort level a model can take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EffortLevel {
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl EffortLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_lowercase().as_str() {
            "minimal" | "min" => Self::Minimal,
            "low" | "light" => Self::Low,
            "medium" | "med" => Self::Medium,
            "high" => Self::High,
            "xhigh" | "extra-high" | "extra_high" => Self::XHigh,
            "max" => Self::Max,
            _ => return None,
        })
    }
}

/// The effort levels a model takes and the one to start on. An empty
/// `options` means the model has no adjustable effort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffortSupport {
    pub options: Vec<EffortLevel>,
    pub default: EffortLevel,
}

impl EffortSupport {
    pub fn supported(&self) -> bool {
        self.options.len() > 1
    }

    fn none(default: EffortLevel) -> Self {
        Self {
            options: Vec::new(),
            default,
        }
    }

    fn of(options: Vec<EffortLevel>, preferred: EffortLevel) -> Self {
        let default = if options.contains(&preferred) {
            preferred
        } else {
            [EffortLevel::Medium, EffortLevel::Low, EffortLevel::High]
                .into_iter()
                .find(|level| options.contains(level))
                .or_else(|| options.first().copied())
                .unwrap_or(preferred)
        };
        Self { options, default }
    }
}

const UP_TO_XHIGH: [EffortLevel; 5] = [
    EffortLevel::Minimal,
    EffortLevel::Low,
    EffortLevel::Medium,
    EffortLevel::High,
    EffortLevel::XHigh,
];

/// Effort support for `bare_model` on the profile `def` describes.
pub fn effort(
    def: &RouteDefinition,
    bare_model: &str,
    catalog: &ModelCatalogCache,
) -> EffortSupport {
    let wire = def.wire_for_model(bare_model);
    let preferred = if wire == WireKind::AnthropicMessages {
        EffortLevel::Low
    } else {
        EffortLevel::Medium
    };
    let prefixed = format!("{}/{bare_model}", def.model_provider_prefix);
    if let Some(values) = catalog.model_effort_options(&prefixed) {
        let options: Vec<EffortLevel> = values
            .iter()
            .filter_map(|value| EffortLevel::parse(value))
            .collect();
        return if options.is_empty() {
            EffortSupport::none(preferred)
        } else {
            EffortSupport::of(options, preferred)
        };
    }
    let model = bare_model.to_ascii_lowercase();
    if def.oauth_flavor == Some(OauthFlavor::Chatgpt) {
        let mut options = UP_TO_XHIGH.to_vec();
        options.push(EffortLevel::Max);
        return EffortSupport::of(options, preferred);
    }
    if def.session_header == Some(SessionHeaderKind::Opencode) {
        return EffortSupport::of(UP_TO_XHIGH.to_vec(), preferred);
    }
    if wire == WireKind::AnthropicMessages {
        const MARKERS: [&str; 6] = [
            "sonnet-5",
            "opus-4-8",
            "opus-4-7",
            "opus-4-6",
            "sonnet-4-6",
            "opus-4-5",
        ];
        if !MARKERS.iter().any(|marker| model.contains(marker)) {
            return EffortSupport::none(preferred);
        }
        let mut options = vec![EffortLevel::Low, EffortLevel::Medium, EffortLevel::High];
        if !(model.contains("4-6") || model.contains("opus-4-5")) {
            options.push(EffortLevel::XHigh);
        }
        return EffortSupport::of(options, preferred);
    }
    let family = match def.effort_family {
        EffortFamily::Openai => ["gpt-5", "o1", "o3", "o4"]
            .iter()
            .any(|prefix| model.starts_with(prefix)),
        EffortFamily::Xai => ["grok-4.3", "grok-4.5", "grok-4.20", "grok-4.6"]
            .iter()
            .any(|marker| model.contains(marker)),
        EffortFamily::None => false,
    };
    if family {
        EffortSupport::of(UP_TO_XHIGH.to_vec(), preferred)
    } else {
        EffortSupport::none(preferred)
    }
}

/// Effort support for a prefixed model id (`anthropic/claude-…`), resolving
/// its profile from the loaded registry by prefix or legacy alias. A model
/// no profile serves has no adjustable effort.
pub fn effort_for_model(model: &str) -> EffortSupport {
    let Some((prefix, bare)) = model.split_once('/') else {
        return EffortSupport::none(EffortLevel::Medium);
    };
    let registry = crate::loaded_registry();
    let profile = registry.profiles().iter().find(|profile| {
        profile.id == prefix
            || profile.model_provider_prefix == prefix
            || crate::normalize::legacy_prefixes(&profile.id).contains(&prefix)
    });
    match profile {
        Some(profile) => effort(
            &RouteDefinition::from_spec(profile),
            bare,
            &ModelCatalogCache::user_default(),
        ),
        None => EffortSupport::none(EffortLevel::Medium),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn support(profile: crate::ProviderSpec, bare: &str) -> EffortSupport {
        let dir = tempdir().unwrap();
        let catalog = ModelCatalogCache::new(dir.path().join("catalog.json"));
        effort(&RouteDefinition::from_spec(&profile), bare, &catalog)
    }

    #[test]
    fn rules_follow_flavor_session_header_wire_and_family() {
        use EffortLevel::*;
        let codex = support(crate::openai_codex_profile(), "gpt-5.6-sol");
        assert_eq!(codex.options, vec![Minimal, Low, Medium, High, XHigh, Max]);
        assert_eq!(codex.default, Medium);

        let go = support(crate::opencode_go_profile(), "kimi-k3");
        assert_eq!(go.options, UP_TO_XHIGH.to_vec());

        let opus = support(crate::anthropic_profile(), "claude-opus-4-8");
        assert_eq!(opus.options, vec![Low, Medium, High, XHigh]);
        assert_eq!(opus.default, Low);
        let sonnet46 = support(crate::anthropic_profile(), "claude-sonnet-4-6");
        assert_eq!(sonnet46.options, vec![Low, Medium, High]);
        assert!(!support(crate::anthropic_profile(), "claude-3-haiku").supported());

        assert!(support(crate::openai_profile(), "gpt-5").supported());
        assert!(support(crate::openai_profile(), "o3-mini").supported());
        assert!(!support(crate::openai_profile(), "gpt-4.1-mini").supported());
        assert!(support(crate::xai_grok_profile(), "grok-4.6").supported());
        assert!(!support(crate::xai_grok_profile(), "grok-3").supported());
        assert!(!support(crate::ollama_profile(), "llama3.2").supported());
    }
}
