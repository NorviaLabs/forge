//! Built-in provider definitions, embedded at compile time from
//! `providers/*.toml` (one file per provider, the same schema as the user's
//! `providers.toml`).

use crate::profile::{ProviderSpec, SpecOrigin};

/// `(file name, contents)` for every built-in definition, sorted by name.
pub const BUILTIN_PROVIDERS: &[(&str, &str)] = &[
    (
        "anthropic.toml",
        include_str!("../providers/anthropic.toml"),
    ),
    ("ollama.toml", include_str!("../providers/ollama.toml")),
    ("openai.toml", include_str!("../providers/openai.toml")),
    ("opencode.toml", include_str!("../providers/opencode.toml")),
    ("xai.toml", include_str!("../providers/xai.toml")),
];

#[derive(serde::Deserialize)]
struct ProvidersFile {
    #[serde(default)]
    providers: Vec<ProviderSpec>,
}

/// Every built-in profile, in file order. A malformed embedded file is a
/// build defect, which `tests/builtin_providers.rs` catches. Parsed once.
pub fn builtin_specs() -> Vec<ProviderSpec> {
    static SPECS: std::sync::OnceLock<Vec<ProviderSpec>> = std::sync::OnceLock::new();
    SPECS.get_or_init(parse_builtin_specs).clone()
}

/// The built-in profile `id`. Panics on an id no embedded file defines,
/// which only a code defect can produce.
pub fn builtin_profile(id: &str) -> ProviderSpec {
    builtin_specs()
        .into_iter()
        .find(|spec| spec.id == id)
        .unwrap_or_else(|| panic!("no built-in provider `{id}`"))
}

/// The built-in profile ids.
pub mod profile_ids {
    pub const ANTHROPIC: &str = "anthropic";
    pub const OLLAMA: &str = "ollama";
    pub const OPENAI: &str = "openai";
    pub const OPENAI_CODEX: &str = "openai_codex";
    pub const OPENCODE_GO: &str = "opencode_go";
    pub const OPENCODE_ZEN: &str = "opencode_zen";
    pub const XAI: &str = "xai";
}

fn parse_builtin_specs() -> Vec<ProviderSpec> {
    BUILTIN_PROVIDERS
        .iter()
        .flat_map(|(name, text)| {
            toml::from_str::<ProvidersFile>(text)
                .unwrap_or_else(|error| panic!("built-in provider file {name}: {error}"))
                .providers
        })
        .map(|mut spec| {
            spec.origin = SpecOrigin::Builtin;
            // A profile's id is its model prefix; lists are stored bare.
            spec.model_provider_prefix = spec.id.clone();
            spec.default_models = crate::normalize::normalize_models(&spec, &spec.default_models);
            spec
        })
        .collect()
}
