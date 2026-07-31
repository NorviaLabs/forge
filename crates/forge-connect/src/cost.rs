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
    profile_id: &str,
    model: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    store: &CredentialStore,
) -> Result<Vec<String>, String> {
    if let Some(report) = model_cost_report(profile_id, model, prompt_tokens, completion_tokens) {
        return Ok(report);
    }

    match profile_id {
        OPENAI_CODEX_PROFILE_ID => codex_cost_report(store),
        "openai" => Ok(unavailable_report(
            "OpenAI API",
            "OpenAI does not expose wallet balance through the API key API.",
            "Usage: https://platform.openai.com/usage · Billing: https://platform.openai.com/settings/organization/billing/overview",
        )),
        "anthropic" => Ok(unavailable_report(
            "Anthropic API",
            "Anthropic does not expose remaining credits through the API key API.",
            "Usage and billing: https://console.anthropic.com/settings/billing",
        )),
        "xai" => Ok(unavailable_report(
            "xAI API",
            "xAI does not expose account balance through the connected OAuth token.",
            "Usage and billing: https://console.x.ai/",
        )),
        "opencode_go" | "opencode_zen" => Ok(unavailable_report(
            "OpenCode",
            "OpenCode does not currently expose wallet balance through its model API.",
            "Usage and billing: https://opencode.ai/",
        )),
        "ollama" => Ok(vec![
            "Provider: Ollama (local)".into(),
            "Cost: local inference; no provider quota or wallet balance.".into(),
        ]),
        other => Ok(vec![
            format!("Provider: {other}"),
            "Limits and billing information are not available for this provider.".into(),
        ]),
    }
}

fn model_cost_report(
    profile_id: &str,
    model: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
) -> Option<Vec<String>> {
    if prompt_tokens == 0 && completion_tokens == 0 {
        return None;
    }
    let provider = models_dev_provider(profile_id)?;
    let model_id = model.split_once('/').map_or(model, |(_, id)| id);
    let cost = fetch_models_dev_cost(provider, model_id).ok().flatten()?;
    let input = token_cost(prompt_tokens, cost.input);
    let output = token_cost(completion_tokens, cost.output);
    let total = input + output;
    Some(vec![
        format!("Provider: {}", provider_title(profile_id)),
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

fn models_dev_provider(profile_id: &str) -> Option<&'static str> {
    match profile_id {
        "openai" => Some("openai"),
        "anthropic" => Some("anthropic"),
        "xai" => Some("xai"),
        "opencode_go" => Some("opencode-go"),
        "opencode_zen" => Some("opencode"),
        _ => None,
    }
}

fn provider_title(profile_id: &str) -> &'static str {
    match profile_id {
        "openai" => "OpenAI API",
        "anthropic" => "Anthropic API",
        "xai" => "xAI API",
        "opencode_go" => "OpenCode Go",
        "opencode_zen" => "OpenCode Zen",
        _ => "Provider",
    }
}

fn token_cost(tokens: u64, dollars_per_million: f64) -> f64 {
    tokens as f64 * dollars_per_million / 1_000_000.0
}

fn fetch_models_dev_cost(provider: &str, model: &str) -> Result<Option<ModelCost>, String> {
    let body: Value = ureq::get(&models_dev_url())
        .set(
            "User-Agent",
            &format!("forge-connect/{}", env!("CARGO_PKG_VERSION")),
        )
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .map_err(|error| format!("models.dev pricing: {error}"))?
        .into_json()
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
        .or_else(|| std::env::var("FORGE_CODEX_ACCESS_TOKEN").ok())
        .ok_or_else(|| "Codex is not connected; use /connect openai_codex".to_string())?;
    let account_id = std::env::var("FORGE_CODEX_ACCOUNT_ID")
        .ok()
        .filter(|id| !id.trim().is_empty())
        .or_else(|| crate::openai_codex::account_id_from_token(&access_token).ok())
        .ok_or_else(|| "Codex credentials do not include a ChatGPT account".to_string())?;

    let response = ureq::get(&codex_usage_url())
        .set("Authorization", &format!("Bearer {access_token}"))
        .set("ChatGPT-Account-Id", &account_id)
        .set(
            "User-Agent",
            &format!("forge/{}", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|error| format!("could not fetch Codex limits: {error}"))?;
    let body = response
        .into_string()
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
            "openai",
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
        for (profile, expected) in [
            ("anthropic", "Anthropic API"),
            ("xai", "xAI API"),
            ("opencode_go", "OpenCode"),
            ("opencode_zen", "OpenCode"),
            ("ollama", "Ollama"),
            ("custom", "custom"),
        ] {
            let report =
                provider_cost_report(profile, "unknown", 0, 0, &CredentialStore::user_default())
                    .unwrap();
            assert!(
                report.iter().any(|line| line.contains(expected)),
                "missing {expected} in {report:?}"
            );
        }
    }

    #[test]
    fn models_dev_provider_and_title_cover_known_profiles() {
        assert_eq!(models_dev_provider("openai"), Some("openai"));
        assert_eq!(models_dev_provider("anthropic"), Some("anthropic"));
        assert_eq!(models_dev_provider("xai"), Some("xai"));
        assert_eq!(models_dev_provider("opencode_go"), Some("opencode-go"));
        assert_eq!(models_dev_provider("opencode_zen"), Some("opencode"));
        assert_eq!(models_dev_provider("ollama"), None);

        assert_eq!(provider_title("openai"), "OpenAI API");
        assert_eq!(provider_title("anthropic"), "Anthropic API");
        assert_eq!(provider_title("xai"), "xAI API");
        assert_eq!(provider_title("opencode_go"), "OpenCode Go");
        assert_eq!(provider_title("opencode_zen"), "OpenCode Zen");
        assert_eq!(provider_title("unknown"), "Provider");
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

    #[test]
    fn codex_cost_report_fetches_usage_from_configured_base_url() {
        use crate::test_env::EnvGuard;
        use forge_test_support::mock_http;
        use tempfile::tempdir;

        const ENV: &[&str] = &[
            crate::openai_codex::API_BASE_ENV,
            crate::openai_codex::ACCESS_TOKEN_ENV,
            crate::openai_codex::ACCOUNT_ID_ENV,
        ];
        let guard = EnvGuard::new(ENV);
        let base = mock_http(vec![(
            200,
            r#"{"plan_type":"plus","rate_limit":{"limit_reached":false,"primary_window":{"used_percent":10}},"credits":{"balance":5.0}}"#,
            vec![],
        )]);
        guard.set(crate::openai_codex::API_BASE_ENV, &base);
        guard.set(crate::openai_codex::ACCESS_TOKEN_ENV, "access-token");
        guard.set(crate::openai_codex::ACCOUNT_ID_ENV, "account-123");

        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let report = codex_cost_report(&store).unwrap();
        assert!(report[0].contains("OpenAI Codex (plus plan)"));
        assert!(report.iter().any(|line| line.contains("90% remaining")));
    }

    #[test]
    fn model_cost_report_uses_models_dev_override_url() {
        use crate::test_env::EnvGuard;
        use forge_test_support::mock_http;

        const ENV: &[&str] = &["FORGE_MODELS_DEV_URL"];
        let guard = EnvGuard::new(ENV);
        let base = mock_http(vec![(
            200,
            r#"{"openai":{"models":{"gpt-4.1-mini":{"cost":{"input":1.0,"output":2.0}}}}}"#,
            vec![],
        )]);
        guard.set("FORGE_MODELS_DEV_URL", &base);

        let report = provider_cost_report(
            "openai",
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
