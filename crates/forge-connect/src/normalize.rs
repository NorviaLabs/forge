//! Legacy model prefixes and aliases, as data.
//!
//! Model ids saved before profile ids became the only identity carry an old
//! prefix (`openai-codex/…`) or alias (`grok/…`). This table says which
//! profile each one belongs to. It is also the source of the reserved ids a
//! custom profile may not take.

use crate::profile::ProviderSpec;

/// `(profile id, old prefixes read as that profile)`.
const LEGACY_PREFIXES: &[(&str, &[&str])] = &[
    (crate::openai::PROFILE_ID, &["openai"]),
    (crate::openai_codex::PROFILE_ID, &["openai-codex"]),
    (crate::anthropic::PROFILE_ID, &["anthropic"]),
    (crate::xai::PROFILE_ID, &["xai", "grok"]),
    (crate::ollama::PROFILE_ID, &["ollama", "ollama_chat"]),
    (crate::opencode_go::PROFILE_ID, &["opencode-go", "opencode"]),
    (crate::opencode_zen::PROFILE_ID, &["opencode-zen"]),
];

/// Old prefixes and aliases read as `profile_id`.
pub(crate) fn legacy_prefixes(profile_id: &str) -> &'static [&'static str] {
    LEGACY_PREFIXES
        .iter()
        .find(|(id, _)| *id == profile_id)
        .map(|(_, prefixes)| *prefixes)
        .unwrap_or(&[])
}

/// Ids a custom profile may not take: every old prefix or alias that is not
/// itself a built-in profile id.
pub(crate) fn is_reserved_id(id: &str) -> bool {
    LEGACY_PREFIXES
        .iter()
        .any(|(profile, prefixes)| *profile != id && prefixes.contains(&id))
}

/// `<PROFILE_ID>` in env names: the id uppercased, `-` replaced by `_`.
pub(crate) fn env_profile_id(id: &str) -> String {
    id.to_ascii_uppercase().replace('-', "_")
}

/// Remove at most one leading `<p>/`, where `p` is the profile's id, its
/// current prefix, or one of its legacy prefixes, then re-prefix with the
/// current `model_provider_prefix`. Other profiles' prefixes are kept, so
/// `meta-llama/Llama-3.3-70B` stays whole for a custom profile.
pub(crate) fn normalize_model(spec: &ProviderSpec, model: &str) -> String {
    let model = model.trim();
    let own = [spec.id.as_str(), spec.model_provider_prefix.as_str()];
    let bare = own
        .iter()
        .chain(legacy_prefixes(&spec.id))
        .filter(|prefix| !prefix.is_empty())
        .find_map(|prefix| {
            model
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('/'))
        })
        .unwrap_or(model);
    format!("{}/{bare}", spec.model_provider_prefix)
}

/// Normalize a model list in place, dropping empties and duplicates.
pub(crate) fn normalize_models(spec: &ProviderSpec, models: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for model in models {
        if model.trim().is_empty() {
            continue;
        }
        let model = normalize_model(spec, model);
        if !out.contains(&model) {
            out.push(model);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_ids_are_old_prefixes_that_are_not_profile_ids() {
        for id in [
            "openai-codex",
            "opencode-go",
            "opencode-zen",
            "grok",
            "ollama_chat",
            "opencode",
        ] {
            assert!(is_reserved_id(id), "{id}");
        }
        for id in ["openai", "xai", "ollama", "anthropic", "my-llm"] {
            assert!(!is_reserved_id(id), "{id}");
        }
    }

    #[test]
    fn models_lose_one_own_prefix_and_take_the_current_one() {
        let go = crate::opencode_go::opencode_go_profile();
        assert_eq!(normalize_model(&go, "opencode/kimi"), "opencode-go/kimi");
        assert_eq!(normalize_model(&go, "opencode-go/kimi"), "opencode-go/kimi");
        assert_eq!(normalize_model(&go, "opencode_go/kimi"), "opencode-go/kimi");
        assert_eq!(normalize_model(&go, "kimi"), "opencode-go/kimi");
        let xai = crate::xai::xai_grok_profile();
        assert_eq!(normalize_model(&xai, "grok/grok-3"), "xai/grok-3");

        let mut custom = crate::profile::test_spec(
            "my-llm",
            crate::AuthSpec::api_key(crate::AuthPrompt::Tui),
            vec![],
        );
        custom.model_provider_prefix = "my-llm".into();
        assert_eq!(
            normalize_model(&custom, "meta-llama/Llama-3.3-70B"),
            "my-llm/meta-llama/Llama-3.3-70B"
        );
        assert_eq!(
            normalize_model(&custom, "my-llm/meta-llama/Llama-3.3-70B"),
            "my-llm/meta-llama/Llama-3.3-70B"
        );
        assert_eq!(
            normalize_models(&custom, &["a".into(), "my-llm/a".into(), " ".into()]),
            vec!["my-llm/a"]
        );
    }

    #[test]
    fn env_profile_ids_uppercase_and_replace_hyphens() {
        assert_eq!(env_profile_id("my-llm"), "MY_LLM");
        assert_eq!(env_profile_id("openai_codex"), "OPENAI_CODEX");
    }
}
