//! Provider setup uses the same stores and verification as the terminal.
//! Secrets never cross IPC; a native prompt collects a missing API key.

use forge_connect::{ConnectService, Credentials, PreferenceStore};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ProviderView {
    pub id: String,
    pub title: String,
    pub auth: String,
    pub connected: bool,
    /// A provider defined by the user, not a built-in.
    pub custom: bool,
    pub base_url: String,
}

#[derive(Debug, Serialize)]
pub struct LoginView {
    pub url: String,
    pub code: String,
}

pub fn list() -> Result<Vec<ProviderView>, String> {
    let registry = forge_connect::loaded_registry();
    let credentials = Credentials::user_default();
    let preferences = PreferenceStore::user_default();
    let service = ConnectService {
        registry: &registry,
        credentials: &credentials,
        preferences: &preferences,
        active_profile_id: None,
        active_model: None,
    };
    let status = service.status().map_err(|error| error.to_string())?;
    Ok(registry
        .profiles()
        .iter()
        .map(|profile| ProviderView {
            id: profile.id.clone(),
            title: profile.title.clone(),
            auth: profile.effective_auth().label().into(),
            connected: status.connected_profile_ids.contains(&profile.id),
            custom: profile.is_user(),
            base_url: profile.base_url.clone(),
        })
        .collect())
}

pub fn connect(
    id: &str,
    emit: impl FnOnce(LoginView) -> Result<(), String>,
) -> Result<String, String> {
    let registry = forge_connect::loaded_registry();
    let credentials = Credentials::user_default();
    let preferences = PreferenceStore::user_default();
    let mut service = ConnectService {
        registry: &registry,
        credentials: &credentials,
        preferences: &preferences,
        active_profile_id: None,
        active_model: None,
    };
    let profile = registry.get(id).ok_or("unknown provider")?;
    let outcome = if profile.effective_auth().is_oauth() {
        match service
            .connect_start_oauth(id)
            .map_err(|error| error.to_string())?
        {
            Ok(outcome) => outcome,
            Err(pending) => {
                emit(login_view(&pending))?;
                if profile.effective_auth().prompt != forge_connect::AuthPrompt::Browser {
                    forge_connect::try_open_browser(pending.open_url());
                }
                service
                    .complete_oauth_device_flow(
                        &pending,
                        std::time::Duration::from_secs(
                            pending.expires_in_secs.unwrap_or(900).min(900),
                        ),
                    )
                    .map_err(|error| error.to_string())?
            }
        }
    } else {
        match service.connect(id, None, false) {
            Ok(outcome) => outcome,
            Err(forge_connect::ConnectError::MissingKey(_)) => {
                let key = prompt_key()?;
                service.connect(id, Some(&key), false).map_err(|_| {
                    "Provider rejected the key or it could not be saved. Check it and retry."
                        .to_string()
                })?
            }
            Err(error) => return Err(error.to_string()),
        }
    };
    let catalog = forge_connect::refresh_profile_catalog(
        profile,
        credentials.store(),
        &forge_connect::ModelCatalogCache::user_default(),
    );
    Ok(if catalog.is_err() {
        "Connected, but the model catalog could not be refreshed. Cached models remain available; add a model ID if needed.".into()
    } else if outcome.verified {
        "Connected. Choose a model to use this provider.".into()
    } else {
        "Saved, but provider verification failed. Check connectivity before using this provider."
            .into()
    })
}

fn login_view(pending: &forge_connect::OauthPending) -> LoginView {
    LoginView {
        url: pending.open_url().into(),
        code: pending.user_code.clone(),
    }
}

pub fn disconnect(id: &str) -> Result<String, String> {
    let registry = forge_connect::loaded_registry();
    let credentials = Credentials::user_default();
    let preferences = PreferenceStore::user_default();
    ConnectService {
        registry: &registry,
        credentials: &credentials,
        preferences: &preferences,
        active_profile_id: None,
        active_model: None,
    }
    .disconnect(Some(id))
    .map_err(|error| error.to_string())
}

fn wire_kind(wire: &str) -> Result<forge_connect::WireKind, String> {
    match wire {
        "" | "chat-completions" => Ok(forge_connect::WireKind::ChatCompletions),
        "responses" => Ok(forge_connect::WireKind::Responses),
        "anthropic-messages" => Ok(forge_connect::WireKind::AnthropicMessages),
        "claude-agent" => Ok(forge_connect::WireKind::ClaudeAgent),
        _ => Err("unknown wire protocol".into()),
    }
}

/// Add a custom provider. The API key, if any, is collected afterwards by the
/// native connect prompt, never over IPC.
pub fn save_custom(id: &str, base_url: &str, model: &str, wire: &str) -> Result<String, String> {
    let profile = forge_connect::save_custom_provider(id, base_url, model, wire_kind(wire)?)?;
    let credentials = Credentials::user_default();
    let preferences = PreferenceStore::user_default();
    let registry = forge_connect::loaded_registry();
    ConnectService {
        registry: &registry,
        credentials: &credentials,
        preferences: &preferences,
        active_profile_id: None,
        active_model: None,
    }
    .clear_provider_state(&forge_connect::ModelCatalogCache::user_default(), &profile);
    Ok(format!(
        "Saved custom provider `{}`. Connect it to add a key.",
        profile.id
    ))
}

pub fn update_custom(id: &str, base_url: &str, model: &str) -> Result<String, String> {
    forge_connect::update_custom_provider(id, base_url, model, None)?;
    Ok(format!("Updated `{id}`."))
}

pub fn remove_custom(id: &str) -> Result<String, String> {
    let registry = forge_connect::loaded_registry();
    let credentials = Credentials::user_default();
    let preferences = PreferenceStore::user_default();
    let report = ConnectService {
        registry: &registry,
        credentials: &credentials,
        preferences: &preferences,
        active_profile_id: None,
        active_model: None,
    }
    .remove_custom_provider(
        &forge_connect::user_providers_path(),
        &forge_connect::ModelCatalogCache::user_default(),
        id,
    )
    .map_err(|error| error.to_string())?;
    let failed = if report.failed.is_empty() {
        String::new()
    } else {
        format!(
            " Some stored state remains: {} entries.",
            report.failed.len()
        )
    };
    Ok(format!("Removed custom provider `{id}`.{failed}"))
}

#[cfg(target_os = "macos")]
fn prompt_key() -> Result<String, String> {
    let output = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", "text returned of (display dialog \"Enter the provider API key. Forge stores it locally; it never enters the webview.\" default answer \"\" with hidden answer buttons {\"Cancel\", \"Connect\"} default button \"Connect\" with title \"Forge — Connect provider\")"])
        .output().map_err(|_| "Cannot open the native key prompt".to_string())?;
    if !output.status.success() {
        return Err("Connection cancelled".into());
    }
    let key = String::from_utf8(output.stdout).map_err(|_| "Invalid key encoding")?;
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err("API key is empty".into());
    }
    Ok(key)
}

#[cfg(not(target_os = "macos"))]
fn prompt_key() -> Result<String, String> {
    Err("Set the provider's API-key environment variable or connect it in Forge's terminal frontend, then retry. Native key entry is currently macOS-only.".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn oauth_view_excludes_private_device_code() {
        let pending = forge_connect::OauthPending::start_stub("xai", "https://auth.x.ai");
        let json = serde_json::to_value(super::login_view(&pending)).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 2);
        assert_eq!(json["code"], pending.user_code);
        assert!(!json.to_string().contains(&pending.device_code));
    }
}
