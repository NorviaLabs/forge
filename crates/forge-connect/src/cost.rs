//! Provider usage, limits, and billing information.

use serde_json::Value;

use crate::{CredentialStore, OPENAI_CODEX_PROFILE_ID};

const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

pub fn provider_cost_report(
    profile_id: &str,
    store: &CredentialStore,
) -> Result<Vec<String>, String> {
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

    let response = ureq::get(CODEX_USAGE_URL)
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
        let report = provider_cost_report("openai", &CredentialStore::user_default()).unwrap();
        assert!(report.iter().any(|line| line.contains("unavailable")));
        assert!(report
            .iter()
            .any(|line| line.contains("platform.openai.com/usage")));
    }
}
