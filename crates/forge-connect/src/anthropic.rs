//! Anthropic connect profile — API key (`anthropic/*`).

use crate::profile::ProviderSpec;
use crate::verify::VerifyError;

pub const PROFILE_ID: &str = crate::builtin::profile_ids::ANTHROPIC;

/// The embedded built-in definition (`providers/*.toml`).
pub fn anthropic_profile() -> ProviderSpec {
    crate::builtin::builtin_profile(PROFILE_ID)
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
            guidance: "Create a key at https://console.anthropic.com/settings/keys.".into(),
        });
    }
    let base = base_url.trim().trim_end_matches('/');
    // Minimal auth check: list models when supported; otherwise POST with invalid body
    // still returns 401 for bad keys vs 400 for good keys.
    let url = format!("{base}/v1/models");
    let resp = ureq::get(&url)
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .header(
            "User-Agent",
            &format!("forge-connect/{}", env!("CARGO_PKG_VERSION")),
        )
        .config()
        // Never follow redirects: a hop could forward the credential.
        .max_redirects(0)
        // Verification branches on the status itself, so a non-2xx is data
        // rather than a transport failure. ureq 3 would otherwise turn every
        // 401/404/500 into `Err` and leave the status arms below unreachable.
        .http_status_as_error(false)
        .timeout_per_call(Some(std::time::Duration::from_secs(15)))
        .build()
        .call();
    match resp {
        Ok(r) if (200..300).contains(&r.status().as_u16()) => Ok(()),
        Ok(r) if r.status().as_u16() == 401 || r.status().as_u16() == 403 => {
            Err(VerifyError::Unauthorized {
                provider: "Anthropic".into(),
                guidance: "Create a key at https://console.anthropic.com/settings/keys.".into(),
            })
        }
        Ok(r) if r.status().as_u16() == 404 => {
            // Older APIs may not expose /v1/models — fall back to a tiny messages call.
            verify_via_messages(key, base)
        }
        Ok(r) => {
            // 400/other with auth accepted is fine for key validity.
            if r.status().as_u16() < 500 {
                Ok(())
            } else {
                Err(VerifyError::Status {
                    provider: "Anthropic".into(),
                    status: r.status().as_u16(),
                    guidance: None,
                })
            }
        }
        Err(other) => Err(VerifyError::Unreachable {
            provider: "Anthropic".into(),
            message: format!("Could not reach Anthropic to verify key ({other}). Check network."),
        }),
    }
}

fn verify_via_messages(key: &str, base: &str) -> Result<(), VerifyError> {
    // Intentionally tiny/invalid payload — we only care about auth status codes.
    let url = format!("{base}/v1/messages");
    let body = r#"{"model":"","max_tokens":1,"messages":[{"role":"user","content":"ping"}]}"#;
    match ureq::post(&url)
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .header(
            "User-Agent",
            &format!("forge-connect/{}", env!("CARGO_PKG_VERSION")),
        )
        .config()
        // Never follow redirects: a hop could forward the credential.
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_per_call(Some(std::time::Duration::from_secs(15)))
        .build()
        .send(body)
    {
        Ok(r) if r.status().as_u16() == 401 || r.status().as_u16() == 403 => {
            Err(VerifyError::Unauthorized {
                provider: "Anthropic".into(),
                guidance: "Create a key at https://console.anthropic.com/settings/keys.".into(),
            })
        }
        // 400/404/etc. means the credential was accepted and the payload was not.
        Ok(_) => Ok(()),
        Err(other) => Err(VerifyError::Unreachable {
            provider: "Anthropic".into(),
            message: format!("Could not reach Anthropic to verify key ({other}). Check network."),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_server(statuses: Vec<u16>) -> Option<String> {
        crate::test_support::serve(statuses, r#"{"ok":true}"#)
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
        let err = verify_api_key("sk-ant-short", &anthropic_profile().base_url)
            .unwrap_err()
            .to_string();
        assert!(err.contains("too short"), "{err}");
    }

    #[test]
    fn empty_key_rejected_without_network() {
        let err = verify_api_key("   ", &anthropic_profile().base_url)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "API key is empty");
    }

    #[test]
    fn verify_models_endpoint_accepts_success_client_error_and_rejects_auth_or_server() {
        let Some(base) = mock_server(vec![200]) else {
            eprintln!("skipping: this host denies binding a mock listener");
            return;
        };
        assert!(verify_api_key("sk-ant-valid-key-for-tests", &base).is_ok());
        assert!(verify_api_key(
            "sk-ant-valid-key-for-tests",
            &mock_server(vec![400]).unwrap()
        )
        .is_ok());

        let err = verify_api_key(
            "sk-ant-valid-key-for-tests",
            &mock_server(vec![403]).unwrap(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unauthorized"), "{err}");
        assert!(!err.contains("sk-ant-valid"));

        // A 500 is now reported as a status failure rather than as
        // "unreachable": the server answered, so the request did reach it.
        let err = verify_api_key(
            "sk-ant-valid-key-for-tests",
            &mock_server(vec![500]).unwrap(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("HTTP 500"), "{err}");
    }

    #[test]
    fn verify_falls_back_to_messages_endpoint_on_missing_models_route() {
        let Some(base) = mock_server(vec![404, 400]) else {
            eprintln!("skipping: this host denies binding a mock listener");
            return;
        };
        assert!(verify_api_key("sk-ant-valid-key-for-tests", &base).is_ok());

        let err = verify_api_key(
            "sk-ant-valid-key-for-tests",
            &mock_server(vec![404, 401]).unwrap(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unauthorized"), "{err}");
        assert!(!err.contains("sk-ant-valid"));
    }
}
