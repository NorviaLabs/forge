//! `providers.toml` compatibility (design §1, "upgrade-only").
//!
//! The fixture is an entry exactly as `save_custom_provider` wrote it before
//! the schema change, with a base-URL override and a prefixed manual model.

use forge_connect::{loaded_registry, loaded_registry_from, save_custom_provider, WireKind};

const LEGACY_ENTRY: &str = r#"[[providers]]
id = "sail-research"
title = "sail-research"
description = "Custom OpenAI-compatible provider"
api_key_env = []
default_base_url = "https://api.sailresearch.com/v1"
default_models = ["sail-research/deepseek-ai/DeepSeek-V4.1-Flash-1017"]
models_dev_providers = []
model_provider_prefix = "sail-research"
vendor_id = "sail-research"
vendor_label = "sail-research"
route_label = ""
route_id = "sail-research"
catalog_mode = "live"
transport = "openai-compat"

[providers.auth_mode.api_key]
tui_always_prompt = true

[provider_overrides."sail-research"]
base_url = "https://edge.sailresearch.com/v1"

[manual_models]
sail-research = ["sail-research/qwen/Qwen3-32B", "deepseek-ai/DeepSeek-V4.1-Flash-1017"]
"#;

#[test]
fn a_legacy_custom_entry_loads_keeps_its_override_and_normalizes_models() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("providers.toml");
    std::fs::write(&path, LEGACY_ENTRY).unwrap();

    let registry = loaded_registry_from(&path);
    assert!(
        registry.load_warnings().is_empty(),
        "{:?}",
        registry.load_warnings()
    );
    let spec = registry.get("sail-research").unwrap();
    assert!(spec.is_user());
    assert_eq!(spec.base_url, "https://edge.sailresearch.com/v1");
    assert_eq!(spec.effective_wire(), WireKind::ChatCompletions);
    assert!(spec.needs_tui_api_key_prompt());
    assert_eq!(
        spec.default_models,
        vec!["sail-research/deepseek-ai/DeepSeek-V4.1-Flash-1017"]
    );
    // The bare duplicate of the default collapses; the prefixed entry stays.
    assert_eq!(spec.manual_models, vec!["sail-research/qwen/Qwen3-32B"]);
}

#[test]
fn an_entry_saved_by_this_writer_reloads() {
    let dir = tempfile::tempdir().unwrap();
    // `save_custom_provider` writes under the user config dir; point it at
    // the temp dir. This test binary has no other test that reads it.
    std::env::set_var("HOME", dir.path());
    std::env::set_var("XDG_CONFIG_HOME", dir.path().join(".config"));

    let saved = save_custom_provider(
        "my-llm",
        "https://llm.example.com/v1",
        "meta-llama/Llama-3.3-70B",
    )
    .unwrap();
    let registry = loaded_registry();
    assert!(
        registry.load_warnings().is_empty(),
        "{:?}",
        registry.load_warnings()
    );
    assert_eq!(registry.get("my-llm"), Some(&saved));
    assert_eq!(
        saved.default_models,
        vec!["my-llm/meta-llama/Llama-3.3-70B"]
    );

    let text = std::fs::read_to_string(find_providers_file(dir.path())).unwrap();
    // Legacy keys stay until stage 8 so an older binary can read the entry.
    for key in [
        "auth_mode",
        "transport = \"openai-compat\"",
        "model_provider_prefix = \"my-llm\"",
        "default_base_url",
        "wire = \"chat-completions\"",
    ] {
        assert!(text.contains(key), "missing `{key}` in:\n{text}");
    }
    assert!(!text.contains("api_key ="), "{text}");
}

/// The `providers.toml` the writer created somewhere under `root`.
fn find_providers_file(root: &std::path::Path) -> std::path::PathBuf {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .is_some_and(|name| name == "providers.toml")
            {
                return path;
            }
        }
    }
    panic!("no providers.toml under {}", root.display());
}
