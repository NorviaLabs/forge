//! Provider usage, limits, and billing information.

use serde_json::Value;

use crate::{CredentialStore, OPENAI_CODEX_PROFILE_ID};

const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const MODELS_DEV_URL: &str = "https://models.dev/api.json";

fn models_dev_url() -> String {
    std::env::var("FORGE_MODELS_DEV_URL").unwrap_or_else(|_| MODELS_DEV_URL.to_string())
}

fn codex_usage_url() -> String {
    std::env::var(crate::openai_codex::API_BASE_ENV)
        .map(|base| format!("{}/wham/usage", base.trim_end_matches('/')))
        .unwrap_or_else(|_| CODEX_USAGE_URL.to_string())
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
        return codex_cost_report(store);
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

fn codex_cost_report(store: &CredentialStore) -> Result<Vec<String>, String> {
    let tokens = store
        .get_oauth(OPENAI_CODEX_PROFILE_ID)
        .map_err(|error| format!("could not read Codex credentials: {error}"))?;
    let access_token = tokens
        .as_ref()
        .map(|tokens| tokens.access_token.trim())
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "Codex is not connected; use /connect openai_codex".to_string())?;
    let account_id = crate::openai_codex::account_id_from_token(&access_token)
        .map_err(|_| "Codex credentials do not include a ChatGPT account".to_string())?;

    let mut response = ureq::get(&codex_usage_url())
        .header("Authorization", &format!("Bearer {access_token}"))
        .header("ChatGPT-Account-Id", &account_id)
        .header(
            "User-Agent",
            &format!("forge/{}", env!("CARGO_PKG_VERSION")),
        )
        .config()
        // Never follow redirects: a hop could forward the credential.
        .max_redirects(0)
        .timeout_per_call(Some(std::time::Duration::from_secs(15)))
        .build()
        .call()
        .map_err(|error| format!("could not fetch Codex limits: {error}"))?;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| format!("could not read Codex limits: {error}"))?;
    let value: Value = serde_json::from_str(&body)
        .map_err(|error| format!("could not parse Codex limits: {error}"))?;
    Ok(format_codex_usage(&value))
}

fn format_codex_usage(value: &Value) -> Vec<String> {
    let plan = value
        .get("plan_type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut lines = vec![format!("Provider: OpenAI Codex ({plan} plan)")];
    let rate_limit = value.get("rate_limit").unwrap_or(&Value::Null);
    if let Some(window) = rate_limit.get("primary_window") {
        lines.push(format_window("Session limit", window));
    }
    if let Some(window) = rate_limit.get("secondary_window") {
        lines.push(format_window("Weekly limit", window));
    }
    if rate_limit
        .get("limit_reached")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        lines.push("Status: limit reached".into());
    }
    if let Some(credits) = value.get("credits") {
        if credits
            .get("unlimited")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            lines.push("Credits: unlimited".into());
        } else if let Some(balance) = credits.get("balance").and_then(number_text) {
            lines.push(format!("Credit balance: {balance}"));
        }
    }
    if lines.len() == 1 {
        lines.push("Limit details were not included by the provider.".into());
    }
    lines
}

fn format_window(label: &str, window: &Value) -> String {
    let used = window
        .get("used_percent")
        .and_then(number_text)
        .unwrap_or_else(|| "?".into());
    let remaining = window
        .get("used_percent")
        .and_then(Value::as_f64)
        .map(|used| format_number((100.0 - used).clamp(0.0, 100.0)))
        .unwrap_or_else(|| "?".into());
    let reset = window
        .get("reset_after_seconds")
        .and_then(Value::as_u64)
        .map(format_duration)
        .unwrap_or_else(|| "unknown".into());
    format!("{label}: {remaining}% remaining ({used}% used) · resets in {reset}")
}

fn number_text(value: &Value) -> Option<String> {
    value.as_f64().map(format_number).or_else(|| {
        value
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    })
}

fn format_number(number: f64) -> String {
    if number.fract().abs() < f64::EPSILON {
        format!("{number:.0}")
    } else {
        format!("{number:.1}")
    }
}

fn format_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_codex_windows_and_credits() {
        let usage = serde_json::json!({
            "plan_type": "plus",
            "rate_limit": {
                "limit_reached": false,
                "primary_window": { "used_percent": 25, "reset_after_seconds": 7200 },
                "secondary_window": { "used_percent": 40.5, "reset_after_seconds": 432000 }
            },
            "credits": { "balance": 12.5, "unlimited": false }
        });
        let report = format_codex_usage(&usage);
        assert_eq!(report[0], "Provider: OpenAI Codex (plus plan)");
        assert!(report[1].contains("75% remaining"));
        assert!(report[2].contains("59.5% remaining"));
        assert_eq!(report[3], "Credit balance: 12.5");
    }

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

    #[test]
    fn codex_usage_handles_limit_reached_unlimited_and_missing_details() {
        let usage = serde_json::json!({
            "plan_type": "team",
            "rate_limit": {
                "limit_reached": true,
                "primary_window": { "used_percent": 125.0 },
                "secondary_window": { "used_percent": "41.5", "reset_after_seconds": 59 }
            },
            "credits": { "unlimited": true }
        });
        let report = format_codex_usage(&usage);
        assert!(report.iter().any(|line| line.contains("0% remaining")));
        assert!(report.iter().any(|line| line == "Status: limit reached"));
        assert!(report.iter().any(|line| line == "Credits: unlimited"));

        let sparse = format_codex_usage(&serde_json::json!({}));
        assert_eq!(sparse[0], "Provider: OpenAI Codex (unknown plan)");
        assert!(sparse[1].contains("not included"));
    }

    #[test]
    fn number_and_duration_formatting_handles_edges() {
        assert_eq!(number_text(&serde_json::json!(12.0)).unwrap(), "12");
        assert_eq!(number_text(&serde_json::json!(12.25)).unwrap(), "12.2");
        assert_eq!(number_text(&serde_json::json!("7.5")).unwrap(), "7.5");
        assert!(number_text(&serde_json::json!("")).is_none());

        assert_eq!(format_duration(59), "0m");
        assert_eq!(format_duration(3_660), "1h 1m");
        assert_eq!(format_duration(90_000), "1d 1h");
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
    fn codex_cost_report_fetches_usage_from_configured_base_url() {
        use crate::test_env::EnvGuard;
        use forge_test_support::mock_http;
        use tempfile::tempdir;

        const ENV: &[&str] = &[crate::openai_codex::API_BASE_ENV];
        let guard = EnvGuard::new(ENV);
        let Some(base) = mock_http(vec![(
            200,
            r#"{"plan_type":"plus","rate_limit":{"limit_reached":false,"primary_window":{"used_percent":10}},"credits":{"balance":5.0}}"#,
            vec![],
        )]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        guard.set(crate::openai_codex::API_BASE_ENV, &base);

        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        store_codex_login(&store);
        let report = codex_cost_report(&store).unwrap();
        assert!(report[0].contains("OpenAI Codex (plus plan)"));
        assert!(report.iter().any(|line| line.contains("90% remaining")));
    }

    #[test]
    fn codex_cost_report_requires_connection() {
        use crate::test_env::EnvGuard;

        const ENV: &[&str] = &[crate::openai_codex::API_BASE_ENV];
        let _guard = EnvGuard::new(ENV);
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let err = codex_cost_report(&store).unwrap_err();
        assert!(err.contains("Codex is not connected"));
    }

    #[test]
    fn provider_cost_report_routes_codex_profile_to_usage_fetch() {
        use crate::test_env::EnvGuard;
        use forge_test_support::mock_http;
        use tempfile::tempdir;

        const ENV: &[&str] = &[crate::openai_codex::API_BASE_ENV];
        let guard = EnvGuard::new(ENV);
        let Some(base) = mock_http(vec![(
            200,
            r#"{"plan_type":"team","rate_limit":{"limit_reached":false,"primary_window":{"used_percent":0}}}"#,
            vec![],
        )]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        guard.set(crate::openai_codex::API_BASE_ENV, &base);

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
