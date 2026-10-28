//! Legacy model prefixes and aliases, as data.
//!
//! Model ids saved before profile ids became the only identity carry an old
//! prefix (`openai-codex/…`) or alias (`grok/…`). This table says which
//! profile each one belongs to. It is also the source of the reserved ids a
//! custom profile may not take.

use crate::profile::{ProviderSpec, WireKind};

/// `(profile id, old prefixes read as that profile)`.
const LEGACY_PREFIXES: &[(&str, &[&str])] = &[
    (crate::builtin::profile_ids::OPENAI, &["openai"]),
    (crate::builtin::profile_ids::OPENAI_CODEX, &["openai-codex"]),
    (crate::builtin::profile_ids::ANTHROPIC, &["anthropic"]),
    (crate::builtin::profile_ids::XAI, &["xai", "grok"]),
    (
        crate::builtin::profile_ids::OLLAMA,
        &["ollama", "ollama_chat"],
    ),
    (
        crate::builtin::profile_ids::OPENCODE_GO,
        &["opencode-go", "opencode"],
    ),
    (crate::builtin::profile_ids::OPENCODE_ZEN, &["opencode-zen"]),
];

/// Built-in route ids sessions stored before the profile id became the only
/// identity, and the profile each one names.
const LEGACY_ROUTES: &[(&str, &str)] = &[
    ("openai-api", crate::builtin::profile_ids::OPENAI),
    ("openai-chatgpt", crate::builtin::profile_ids::OPENAI_CODEX),
    ("anthropic-api", crate::builtin::profile_ids::ANTHROPIC),
    ("xai-api", crate::builtin::profile_ids::XAI),
    ("opencode-go", crate::builtin::profile_ids::OPENCODE_GO),
    ("opencode-zen", crate::builtin::profile_ids::OPENCODE_ZEN),
];

/// The profile id a stored route names: a built-in's old route id maps to
/// its profile; anything else (a custom id, `ollama`) is returned as is.
pub fn profile_id_for_stored_route(route: &str) -> &str {
    let route = route.trim();
    LEGACY_ROUTES
        .iter()
        .find(|(old, _)| *old == route)
        .map(|(_, id)| *id)
        .unwrap_or(route)
}

/// A hint for a model id under an old prefix: the id it should use now.
pub fn old_prefix_hint(model: &str) -> Option<String> {
    let normalized = normalize_saved_model_id(model);
    (normalized != model.trim()).then_some(normalized)
}

/// Old prefixes and aliases read as `profile_id`.
pub fn legacy_prefixes(profile_id: &str) -> &'static [&'static str] {
    LEGACY_PREFIXES
        .iter()
        .find(|(id, _)| *id == profile_id)
        .map(|(_, prefixes)| *prefixes)
        .unwrap_or(&[])
}

/// Ids a custom profile may not take: every old prefix, alias, or built-in
/// route id that is not itself a built-in profile id (stored sessions map
/// those to built-ins).
pub(crate) fn is_reserved_id(id: &str) -> bool {
    LEGACY_PREFIXES
        .iter()
        .any(|(profile, prefixes)| *profile != id && prefixes.contains(&id))
        || LEGACY_ROUTES
            .iter()
            .any(|(route, profile)| *route == id && *profile != id)
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
    let own = [
        spec.id.as_str(),
        spec.model_provider_prefix.as_str(),
        spec.legacy_prefix.as_deref().unwrap_or(""),
    ];
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

/// A saved `<prefix>/<model>` id in its current form: an old built-in
/// prefix or alias (`openai-codex/`, `grok/`, …) becomes that profile's id.
/// Other ids are returned unchanged.
pub fn normalize_saved_model_id(model: &str) -> String {
    let model = model.trim();
    let Some((prefix, bare)) = model.split_once('/') else {
        return model.to_string();
    };
    LEGACY_PREFIXES
        .iter()
        .find(|(_, prefixes)| prefixes.contains(&prefix))
        .map(|(id, _)| format!("{id}/{bare}"))
        .unwrap_or_else(|| model.to_string())
}

/// The wire a legacy `transport` value maps to.
pub(crate) fn wire_from_legacy_transport(transport: &str) -> Option<WireKind> {
    match transport {
        "openai-compat" => Some(WireKind::ChatCompletions),
        "anthropic" => Some(WireKind::AnthropicMessages),
        "codex" => Some(WireKind::Responses),
        _ => None,
    }
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
        let go = crate::builtin::builtin_profile("opencode_go");
        assert_eq!(normalize_model(&go, "opencode/kimi"), "opencode_go/kimi");
        assert_eq!(normalize_model(&go, "opencode-go/kimi"), "opencode_go/kimi");
        assert_eq!(normalize_model(&go, "opencode_go/kimi"), "opencode_go/kimi");
        assert_eq!(normalize_model(&go, "kimi"), "opencode_go/kimi");
        let xai = crate::builtin::builtin_profile("xai");
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
    fn saved_ids_under_old_prefixes_read_as_the_profile_id() {
        assert_eq!(
            normalize_saved_model_id("openai-codex/gpt-5"),
            "openai_codex/gpt-5"
        );
        assert_eq!(
            normalize_saved_model_id("opencode/kimi"),
            "opencode_go/kimi"
        );
        assert_eq!(normalize_saved_model_id("opencode-zen/x"), "opencode_zen/x");
        assert_eq!(normalize_saved_model_id("grok/grok-4"), "xai/grok-4");
        assert_eq!(normalize_saved_model_id("openai/gpt-5"), "openai/gpt-5");
        assert_eq!(normalize_saved_model_id("my-llm/org/m"), "my-llm/org/m");
        assert_eq!(normalize_saved_model_id("bare"), "bare");
    }

    #[test]
    fn env_profile_ids_uppercase_and_replace_hyphens() {
        assert_eq!(env_profile_id("my-llm"), "MY_LLM");
        assert_eq!(env_profile_id("openai_codex"), "OPENAI_CODEX");
    }
}
