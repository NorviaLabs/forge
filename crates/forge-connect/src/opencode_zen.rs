//! OpenCode Zen connect profile — API key + TUI always prompt.
//!
//! OpenCode Zen is OpenAI-compatible at `https://opencode.ai/zen/v1` (pay-per-use catalog).
//! The same account API key works for Zen and Go; bases differ.
//! Native routing maps `opencode-zen/<id>` to the Zen OpenAI-compatible endpoint.

use crate::profile::ProviderSpec;

pub const PROFILE_ID: &str = crate::builtin::profile_ids::OPENCODE_ZEN;

/// OpenAI-compatible chat/completions base for Zen (not the Go subscription path).
pub const DEFAULT_BASE_URL: &str = "https://opencode.ai/zen/v1";

/// Env var the worker reads for the Zen API base (set by connect).
pub const API_BASE_ENV: &str = "OPENCODE_ZEN_API_BASE";

/// The embedded built-in definition (`providers/*.toml`).
pub fn opencode_zen_profile() -> ProviderSpec {
    crate::builtin::builtin_profile(PROFILE_ID)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_shape() {
        let p = opencode_zen_profile();
        assert_eq!(p.id, "opencode_zen");
        assert!(p.needs_tui_api_key_prompt());
        assert!(p.base_url.ends_with("/zen/v1"));
        assert!(!p.base_url.contains("/go/"));
        assert!(p
            .default_models
            .iter()
            .all(|m| m.starts_with("opencode_zen/")));
    }
}
