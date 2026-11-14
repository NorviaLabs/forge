//! Model tiers: Forge's own names for a provider/model/effort combination.
//!
//! A tier is not a model id. It is a request — "something fast", "the usual",
//! "the strongest available" — that Forge resolves against the provider a
//! session is already using (see `forge_connect::tiers`). Users never
//! configure tiers; they are the groundwork for choosing models on the user's
//! behalf.

use serde::{Deserialize, Serialize};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// The cheapest capable model, at low effort.
    Fast,
    /// The provider's everyday model, at its default effort.
    Default,
    /// The most capable model, at high effort.
    Strong,
}

impl ModelTier {
    pub const ALL: [ModelTier; 3] = [ModelTier::Fast, ModelTier::Default, ModelTier::Strong];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Default => "default",
            Self::Strong => "strong",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "fast" => Some(Self::Fast),
            "default" => Some(Self::Default),
            "strong" => Some(Self::Strong),
            _ => None,
        }
    }
}

impl std::fmt::Display for ModelTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::ModelTier;

    #[test]
    fn tiers_round_trip_through_text_and_serde() {
        for tier in ModelTier::ALL {
            assert_eq!(ModelTier::parse(tier.as_str()), Some(tier));
            let json = serde_json::to_value(tier).unwrap();
            assert_eq!(json, serde_json::json!(tier.as_str()));
            assert_eq!(serde_json::from_value::<ModelTier>(json).unwrap(), tier);
        }
        assert_eq!(ModelTier::parse(" Strong "), Some(ModelTier::Strong));
        assert_eq!(ModelTier::parse("turbo"), None);
    }
}
