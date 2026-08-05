//! Anthropic connect profile — API key (`anthropic/*`).

use crate::auth::AuthMode;
use crate::profile::ConnectProfile;
use crate::verify::VerifyError;

pub const PROFILE_ID: &str = "anthropic";
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

pub fn anthropic_profile() -> ConnectProfile {
    ConnectProfile {
        id: PROFILE_ID.into(),
        title: "Anthropic".into(),
        description: "Anthropic API key — anthropic/* (Claude) models".into(),
        auth_mode: AuthMode::ApiKey {
            tui_always_prompt: true,
        },
        api_key_env: vec!["ANTHROPIC_API_KEY".into()],
        default_base_url: Some(DEFAULT_BASE_URL.into()),
        default_models: vec!["anthropic/claude-sonnet-4-20250514".into()],
        models_dev_providers: vec!["anthropic".into()],
        auth_url: Some("https://console.anthropic.com/settings/keys".into()),
        model_provider_prefix: "anthropic".into(),
        vendor_id: PROFILE_ID.into(),
        vendor_label: "Anthropic".into(),
        route_label: String::new(),
    }
}

/// Verify key with a lightweight messages probe (or models if available).
/// Uses Anthropic's `x-api-key` header. Never includes the key in errors.
pub fn verify_api_key(api_key: &str, base_url: &str) -> Result<(), VerifyError> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err(VerifyError::EmptyKey);
    }
    if key.len() < 16 {
        return Err(VerifyError::KeyTooShort {
            len: key.len(),
            guidance: "Create a key at https://console.anthropic.com/settings/keys.",
        });
    }
    let base = base_url.trim().trim_end_matches('/');
    // Minimal auth check: list models when supported; otherwise POST with invalid body
    // still returns 401 for bad keys vs 400 for good keys.
    let url = format!("{base}/v1/models");
    let resp = ureq::get(&url)
        .set("x-api-key", key)
        .set("anthropic-version", "2023-06-01")
        .set(
            "User-Agent",
            &format!("forge-connect/{}", env!("CARGO_PKG_VERSION")),
        )
        .timeout(std::time::Duration::from_secs(15))
        .call();
    match resp {
        Ok(r) if (200..300).contains(&r.status()) => Ok(()),
        Ok(r) if r.status() == 401 || r.status() == 403 => Err(VerifyError::Unauthorized {
            provider: "Anthropic",
            guidance: "Create a key at https://console.anthropic.com/settings/keys.",
        }),
        Ok(r) if r.status() == 404 => {
            // Older APIs may not expose /v1/models — fall back to a tiny messages call.
            verify_via_messages(key, base)
        }
        Ok(r) => {
            // 400/other with auth accepted is fine for key validity.
            if r.status() < 500 {
                Ok(())
            } else {
                Err(VerifyError::Status {
                    provider: "Anthropic",
                    status: r.status(),
                    guidance: None,
                })
            }
        }
        Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => {
            Err(VerifyError::Unauthorized {
                provider: "Anthropic",
                guidance: "Create a key at https://console.anthropic.com/settings/keys.",
            })
        }
        Err(ureq::Error::Status(404, _)) => verify_via_messages(key, base),
        Err(ureq::Error::Status(code, _)) if code < 500 => Ok(()),
        Err(other) => Err(VerifyError::Unreachable {
            provider: "Anthropic",
            message: format!("Could not reach Anthropic to verify key ({other}). Check network."),
        }),
    }
}

fn verify_via_messages(key: &str, base: &str) -> Result<(), VerifyError> {
    // Intentionally tiny/invalid payload — we only care about auth status codes.
    let url = format!("{base}/v1/messages");
    let body = r#"{"model":"","max_tokens":1,"messages":[{"role":"user","content":"ping"}]}"#;
    match ureq::post(&url)
        .set("x-api-key", key)
        .set("anthropic-version", "2023-06-01")
        .set("content-type", "application/json")
        .set(
            "User-Agent",
            &format!("forge-connect/{}", env!("CARGO_PKG_VERSION")),
        )
        .timeout(std::time::Duration::from_secs(15))
        .send_string(body)
    {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(401, _)) | Err(ureq::Error::Status(403, _)) => {
            Err(VerifyError::Unauthorized {
                provider: "Anthropic",
                guidance: "Create a key at https://console.anthropic.com/settings/keys.",
            })
        }
        Err(ureq::Error::Status(_, _)) => Ok(()), // 400/404/etc. means auth passed
        Err(other) => Err(VerifyError::Unreachable {
            provider: "Anthropic",
            message: format!("Could not reach Anthropic to verify key ({other}). Check network."),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn mock_server(statuses: Vec<u16>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for status in statuses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0_u8; 1024];
                let _ = stream.read(&mut buf);
                let body = r#"{"ok":true}"#;
                let response = format!(
                    "HTTP/1.1 {status} test\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        format!("http://{addr}/")
    }

    #[test]
    fn profile_shape() {
        let p = anthropic_profile();
        assert_eq!(p.id, "anthropic");
        assert!(p.needs_tui_api_key_prompt());
        assert!(p.default_models.iter().all(|m| m.starts_with("anthropic/")));
    }

    #[test]
    fn short_key_rejected() {
        let err = verify_api_key("sk-ant-short", DEFAULT_BASE_URL)
            .unwrap_err()
            .to_string();
        assert!(err.contains("too short"), "{err}");
    }

    #[test]
    fn empty_key_rejected_without_network() {
        let err = verify_api_key("   ", DEFAULT_BASE_URL)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "API key is empty");
    }

    #[test]
    fn verify_models_endpoint_accepts_success_client_error_and_rejects_auth_or_server() {
        assert!(verify_api_key("sk-ant-valid-key-for-tests", &mock_server(vec![200])).is_ok());
        assert!(verify_api_key("sk-ant-valid-key-for-tests", &mock_server(vec![400])).is_ok());

        let err = verify_api_key("sk-ant-valid-key-for-tests", &mock_server(vec![403]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unauthorized"), "{err}");
        assert!(!err.contains("sk-ant-valid"));

        let err = verify_api_key("sk-ant-valid-key-for-tests", &mock_server(vec![500]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("status code 500"), "{err}");
    }

    #[test]
    fn verify_falls_back_to_messages_endpoint_on_missing_models_route() {
        assert!(verify_api_key("sk-ant-valid-key-for-tests", &mock_server(vec![404, 400])).is_ok());

        let err = verify_api_key("sk-ant-valid-key-for-tests", &mock_server(vec![404, 401]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unauthorized"), "{err}");
        assert!(!err.contains("sk-ant-valid"));
    }
}
