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
/// build defect, which `tests/builtin_providers.rs` catches.
pub fn builtin_specs() -> Vec<ProviderSpec> {
    BUILTIN_PROVIDERS
        .iter()
        .flat_map(|(name, text)| {
            toml::from_str::<ProvidersFile>(text)
                .unwrap_or_else(|error| panic!("built-in provider file {name}: {error}"))
                .providers
        })
        .map(|mut spec| {
            spec.origin = SpecOrigin::Builtin;
            spec
        })
        .collect()
}
