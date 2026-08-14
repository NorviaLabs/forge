//! Ollama connect profile — local OpenAI-compatible LLM (`ollama/*`).
//!
//! No cloud API key required. Connect checks that the Ollama daemon is reachable.

use crate::auth::AuthMode;
use crate::profile::{CatalogMode, ProviderSpec, ProviderTransport, SpecOrigin};
use crate::verify::VerifyError;

pub const PROFILE_ID: &str = "ollama";
/// Default local Ollama OpenAI-compatible base.
pub const DEFAULT_BASE_URL: &str = "http://localhost:11434";
pub const API_BASE_ENV: &str = "OLLAMA_API_BASE";
/// Stored / exported placeholder when no optional key is set (Ollama ignores it).
pub const LOCAL_PLACEHOLDER_KEY: &str = "ollama";

pub fn ollama_profile() -> ProviderSpec {
    ProviderSpec {
        id: PROFILE_ID.into(),
        title: "Ollama (local)".into(),
        description: "Local Ollama models — no API key required (ollama/*)".into(),
        // No mandatory TUI key prompt: local server auth is optional.
        auth_mode: AuthMode::ApiKey {
            tui_always_prompt: false,
        },
        api_key_env: vec!["OLLAMA_API_KEY".into()],
        default_base_url: Some(DEFAULT_BASE_URL.into()),
        default_models: vec![],
        models_dev_providers: vec![],
        auth_url: Some("https://ollama.com/download".into()),
        model_provider_prefix: "ollama".into(),
        vendor_id: PROFILE_ID.into(),
        vendor_label: "Ollama".into(),
        route_label: "Local".into(),
        route_id: "ollama".into(),
        catalog_mode: CatalogMode::Live,
        transport: ProviderTransport::OpenaiCompat,
        origin: SpecOrigin::Builtin,
    }
}

/// Probe `GET {base}/api/tags` to ensure Ollama is running.
pub fn verify_reachable(base_url: &str) -> Result<(), VerifyError> {
    let base = base_url.trim().trim_end_matches('/');
    // Prefer native tags API; fall back to OpenAI-compatible /v1/models.
    let tags = format!("{base}/api/tags");
    match ureq::get(&tags)
        .set(
            "User-Agent",
            &format!("forge-connect/{}", env!("CARGO_PKG_VERSION")),
        )
        .timeout(std::time::Duration::from_secs(3))
        .call()
    {
        Ok(r) if (200..300).contains(&r.status()) => Ok(()),
        Ok(r) => Err(VerifyError::Unhealthy {
            provider: "Ollama",
            status: r.status(),
            endpoint: base.to_string(),
            guidance: "Is the server healthy?",
        }),
        Err(_) => {
            let models = format!("{base}/v1/models");
            match ureq::get(&models)
                .set(
                    "User-Agent",
                    &format!("forge-connect/{}", env!("CARGO_PKG_VERSION")),
                )
                .timeout(std::time::Duration::from_secs(3))
                .call()
            {
                Ok(r) if (200..300).contains(&r.status()) => Ok(()),
                Ok(r) => Err(VerifyError::Unhealthy {
                    provider: "Ollama",
                    status: r.status(),
                    endpoint: base.to_string(),
                    guidance: "Is the server healthy?",
                }),
                Err(e) => Err(VerifyError::Unreachable {
                    provider: "Ollama",
                    message: format!(
                        "Cannot reach Ollama at {base} ({e}). \
Start it with `ollama serve` (default http://localhost:11434)."
                    ),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_server(statuses: Vec<u16>) -> String {
        crate::test_support::serve(statuses, r#"{"models":[]}"#)
    }

    #[test]
    fn profile_shape() {
        let p = ollama_profile();
        assert_eq!(p.id, "ollama");
        assert!(!p.needs_tui_api_key_prompt());
        assert!(p.default_models.is_empty());
        assert!(p.default_base_url.as_deref().unwrap().contains("11434"));
    }

    #[test]
    fn verify_reachable_accepts_tags_and_reports_unhealthy_status() {
        assert!(verify_reachable(&mock_server(vec![200])).is_ok());

        let err = verify_reachable(&mock_server(vec![503, 500]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("status code 500"), "{err}");
    }

    #[test]
    fn verify_reachable_falls_back_to_openai_models_route() {
        assert!(verify_reachable(&mock_server(vec![404, 200])).is_ok());

        let err = verify_reachable(&mock_server(vec![404, 500]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("status code 500"), "{err}");
    }
}
