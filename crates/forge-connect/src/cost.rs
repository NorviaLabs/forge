//! Provider usage, limits, and billing information.

use serde_json::Value;

use crate::CredentialStore;

const MODELS_DEV_URL: &str = "https://models.dev/api.json";

fn models_dev_url() -> String {
    std::env::var("FORGE_MODELS_DEV_URL").unwrap_or_else(|_| MODELS_DEV_URL.to_string())
}

pub fn provider_cost_report(
    profile: &crate::ProviderSpec,
    model: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    store: &CredentialStore,
) -> Result<Vec<String>, String> {
    // The ChatGPT flavor owns its usage endpoint (until stage 6 moves it).
    if profile.effective_auth().flavor == Some(crate::OauthFlavor::Chatgpt) {
        return crate::oauth_chatgpt::usage_report(store);
    }
    if let Some(report) = model_cost_report(profile, model, prompt_tokens, completion_tokens) {
        return Ok(report);
    }
    let title = profile.title.as_str();
    let note = profile.billing_note.trim();
    let url = profile.billing_url.trim();
    Ok(match (note.is_empty(), url.is_empty()) {
        (false, false) => unavailable_report(title, note, &format!("Usage and billing: {url}")),
        (false, true) => vec![format!("Provider: {title}"), note.to_string()],
        (true, false) => vec![
            format!("Provider: {title}"),
            format!("Usage and billing: {url}"),
        ],
        (true, true) => vec![
            format!("Provider: {title}"),
            "Limits and billing information are not available for this provider.".into(),
        ],
    })
}

fn model_cost_report(
    profile: &crate::ProviderSpec,
    model: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
) -> Option<Vec<String>> {
    if prompt_tokens == 0 && completion_tokens == 0 {
        return None;
    }
    let provider = profile.models_dev_providers.first()?;
    let model_id = model.split_once('/').map_or(model, |(_, id)| id);
    let cost = fetch_models_dev_cost(provider, model_id).ok().flatten()?;
    let input = token_cost(prompt_tokens, cost.input);
    let output = token_cost(completion_tokens, cost.output);
    let total = input + output;
    Some(vec![
        format!("Provider: {}", profile.title),
        format!("Estimated session cost: ${total:.6} (input ${input:.6} + output ${output:.6})"),
        format!(
            "Rates: ${:.2}/M input · ${:.2}/M output · source: models.dev",
            cost.input, cost.output
        ),
    ])
}

#[derive(Debug, Clone, Copy)]
struct ModelCost {
    input: f64,
    output: f64,
}

fn token_cost(tokens: u64, dollars_per_million: f64) -> f64 {
    tokens as f64 * dollars_per_million / 1_000_000.0
}

fn fetch_models_dev_cost(provider: &str, model: &str) -> Result<Option<ModelCost>, String> {
    let body: Value = ureq::get(&models_dev_url())
        .header(
            "User-Agent",
            &format!("forge-connect/{}", env!("CARGO_PKG_VERSION")),
        )
        .config()
        // Never follow redirects: a hop could forward the credential.
        .max_redirects(0)
        .timeout_per_call(Some(std::time::Duration::from_secs(10)))
        .build()
        .call()
        .map_err(|error| format!("models.dev pricing: {error}"))?
        .body_mut()
        .read_json()
        .map_err(|error| format!("models.dev pricing JSON: {error}"))?;
    Ok(models_dev_cost_from_value(&body, provider, model))
}

fn models_dev_cost_from_value(body: &Value, provider: &str, model: &str) -> Option<ModelCost> {
    let costs = body.get(provider)?.get("models")?.get(model)?.get("cost")?;
    Some(ModelCost {
        input: costs.get("input").and_then(Value::as_f64).unwrap_or(0.0),
        output: costs.get("output").and_then(Value::as_f64).unwrap_or(0.0),
    })
}

fn unavailable_report(title: &str, reason: &str, link: &str) -> Vec<String> {
    vec![
        format!("Provider: {title}"),
        format!("Balance: unavailable — {reason}"),
        link.into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OPENAI_CODEX_PROFILE_ID;

    #[test]
    fn api_key_provider_explains_balance_limitation() {
        let report = provider_cost_report(
            &crate::openai_profile(),
            "openai/unknown",
            0,
            0,
            &CredentialStore::user_default(),
        )
        .unwrap();
        assert!(report.iter().any(|line| line.contains("unavailable")));
        assert!(report
            .iter()
            .any(|line| line.contains("platform.openai.com/usage")));
    }

    #[test]
    fn estimates_model_cost_from_models_dev_shape() {
        let body = serde_json::json!({
            "opencode": {
                "models": {
                    "claude-sonnet-4-5": {
                        "cost": { "input": 3.0, "output": 15.0, "cache_read": 0.3 }
                    }
                }
            }
        });
        let cost = models_dev_cost_from_value(&body, "opencode", "claude-sonnet-4-5").unwrap();
        assert_eq!(token_cost(1_000_000, cost.input), 3.0);
        assert_eq!(token_cost(2_000_000, cost.output), 30.0);
    }

    #[test]
    fn provider_cost_report_covers_static_provider_messages() {
        let mut custom = crate::openai_profile();
        custom.title = "custom".into();
        custom.billing_note.clear();
        custom.billing_url.clear();
        for (profile, expected) in [
            (crate::anthropic_profile(), "remaining credits"),
            (crate::xai_grok_profile(), "xAI does not expose"),
            (crate::opencode_go_profile(), "OpenCode does not"),
            (crate::opencode_zen_profile(), "OpenCode does not"),
            (crate::ollama_profile(), "local inference"),
            (custom, "custom"),
        ] {
            let report =
                provider_cost_report(&profile, "unknown", 0, 0, &CredentialStore::user_default())
                    .unwrap();
            assert!(
                report.iter().any(|line| line.contains(expected)),
                "missing {expected} in {report:?}"
            );
        }
    }

    /// A stored ChatGPT login whose token names account `account-123`.
    fn store_codex_login(store: &CredentialStore) {
        store
            .set_oauth(
                OPENAI_CODEX_PROFILE_ID,
                crate::OauthTokens {
                    access_token: "header.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjb3VudC0xMjMifX0.signature".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
    }

    #[test]
    fn provider_cost_report_routes_codex_profile_to_usage_fetch() {
        use crate::test_env::EnvGuard;
        use forge_test_support::mock_http;
        use tempfile::tempdir;

        const ENV: &[&str] = &[crate::oauth_chatgpt::API_BASE_ENV];
        let guard = EnvGuard::new(ENV);
        let Some(base) = mock_http(vec![(
            200,
            r#"{"plan_type":"team","rate_limit":{"limit_reached":false,"primary_window":{"used_percent":0}}}"#,
            vec![],
        )]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        guard.set(crate::oauth_chatgpt::API_BASE_ENV, &base);

        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        store_codex_login(&store);
        let report = provider_cost_report(
            &crate::openai_codex_profile(),
            "openai-codex/gpt-test",
            0,
            0,
            &store,
        )
        .unwrap();
        assert!(report[0].contains("OpenAI Codex (team plan)"));
    }

    #[test]
    fn model_cost_report_uses_models_dev_override_url() {
        use crate::test_env::EnvGuard;
        use forge_test_support::mock_http;

        const ENV: &[&str] = &["FORGE_MODELS_DEV_URL"];
        let guard = EnvGuard::new(ENV);
        let Some(base) = mock_http(vec![(
            200,
            r#"{"openai":{"models":{"gpt-4.1-mini":{"cost":{"input":1.0,"output":2.0}}}}}"#,
            vec![],
        )]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        guard.set("FORGE_MODELS_DEV_URL", &base);

        let report = provider_cost_report(
            &crate::openai_profile(),
            "openai/gpt-4.1-mini",
            1_000_000,
            500_000,
            &CredentialStore::user_default(),
        )
        .unwrap();
        assert!(report
            .iter()
            .any(|line| line.contains("Estimated session cost")));
        assert!(report.iter().any(|line| line.contains("$2.000000")));
    }
}
