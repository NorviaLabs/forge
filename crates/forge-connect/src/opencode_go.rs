//! OpenCode Go connect profile — API key + TUI always prompt (PROV-02 / Phase 6.1).
//!
//! OpenCode Go is OpenAI-compatible at `https://opencode.ai/zen/go/v1`.
//! Native routing maps `opencode-go/<id>` to this OpenAI-compatible endpoint.

use crate::auth::AuthMode;
use crate::profile::ConnectProfile;

pub const PROFILE_ID: &str = "opencode_go";

/// OpenAI-compatible chat/completions base for the Go subscription.
pub const DEFAULT_BASE_URL: &str = "https://opencode.ai/zen/go/v1";

/// Env var the worker reads for the Go API base (set by connect).
pub const API_BASE_ENV: &str = "OPENCODE_API_BASE";

pub fn opencode_go_profile() -> ConnectProfile {
    ConnectProfile {
        id: PROFILE_ID.into(),
        title: "OpenCode Go".into(),
        description: "OpenCode Go coding models — API key required (TUI prompts)".into(),
        auth_mode: AuthMode::opencode_go_api_key(),
        api_key_env: vec!["OPENCODE_API_KEY".into(), "OPENCODE_GO_API_KEY".into()],
        default_base_url: Some(DEFAULT_BASE_URL.into()),
        // Distinctive prefix avoids hijacking real OpenAI (`openai/gpt-*`) routes.
        default_models: vec!["opencode-go/gpt-4.1-mini".into()],
        models_dev_providers: vec![],
        auth_url: Some("https://opencode.ai/auth".into()),
        model_provider_prefix: "opencode-go".into(),
        vendor_id: "opencode".into(),
        vendor_label: "OpenCode".into(),
        route_label: "Go".into(),
    }
}

/// Live-check an OpenCode Go API key against `GET {base}/models`.
/// Returns Ok(()) on 200; never includes the key in error messages.
pub fn verify_api_key(api_key: &str, base_url: &str) -> Result<(), crate::verify::VerifyError> {
    use crate::verify::VerifyError;
    let key = api_key.trim();
    if key.is_empty() {
        return Err(VerifyError::EmptyKey);
    }
    // Real Zen/Go keys are long; reject obvious placeholders without a network call.
    if key.len() < 16 {
        return Err(VerifyError::KeyTooShort {
            len: key.len(),
            guidance: "Get a key from https://opencode.ai/auth (OpenCode Zen → subscribe to Go → copy API key).",
        });
    }
    let base = base_url.trim().trim_end_matches('/');
    let url = format!("{base}/models");
    let resp = ureq::get(&url)
        .set("Authorization", &format!("Bearer {key}"))
        .set(
            "User-Agent",
            &format!("forge-connect/{}", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(status, _) => VerifyError::Rejected {
                provider: "OpenCode Go",
                status,
                guidance: "Sign in at https://opencode.ai/auth, copy a fresh key, and reconnect.",
            },
            other => VerifyError::Unreachable {
                provider: "OpenCode Go",
                message: format!(
                    "Could not reach OpenCode Go to verify key ({other}). \
Check network access to {base}."
                ),
            },
        })?;
    let status = resp.status();
    if (200..300).contains(&status) {
        Ok(())
    } else if status == 401 || status == 403 {
        Err(VerifyError::Unauthorized {
            provider: "OpenCode Go",
            guidance: "Get a key from https://opencode.ai/auth and use `/connect opencode_go --key …` in the TUI.",
        })
    } else {
        Err(VerifyError::Status {
            provider: "OpenCode Go",
            status,
            guidance: Some("Try again or check account status."),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ConnectRegistry;
    use crate::service::{
        handle_connect_action, needs_tui_api_key_prompt, ConnectAction, ConnectError,
    };
    use crate::store::CredentialStore;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use tempfile::tempdir;

    fn mock_server(status: u16) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0_u8; 1024];
            let _ = stream.read(&mut buf);
            let body = r#"{"data":[]}"#;
            let response = format!(
                "HTTP/1.1 {status} test\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        format!("http://{addr}/")
    }

    #[test]
    fn profile_always_prompts_for_api_key() {
        let p = opencode_go_profile();
        assert_eq!(p.id, "opencode_go");
        assert!(p.auth_mode.is_api_key());
        assert!(p.needs_tui_api_key_prompt());
        assert!(!p.rejects_api_key_cli());
        assert!(p.auth_url.as_deref().unwrap().contains("opencode.ai"));
    }

    #[test]
    fn tui_flag_true_for_opencode_go() {
        let mut reg = ConnectRegistry::new();
        reg.register(opencode_go_profile());
        assert!(needs_tui_api_key_prompt(&reg, "opencode_go"));
    }

    #[test]
    fn connect_requires_key() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let mut reg = ConnectRegistry::new();
        reg.register(opencode_go_profile());
        let mut ap = None;
        let mut am = None;
        // The guard clears these, isolating the test from the developer's shell.
        let _guard = crate::test_env::EnvGuard::new(&[
            "OPENCODE_API_KEY",
            "OPENCODE_GO_API_KEY",
            "FORGE_CONNECT_SKIP_VERIFY",
        ]);
        let err = handle_connect_action(
            ConnectAction::Connect {
                profile_id: "opencode_go".into(),
                api_key: None,
                oauth_fixture: false,
            },
            &reg,
            &store,
            &mut ap,
            &mut am,
        )
        .unwrap_err();
        assert!(matches!(err, ConnectError::MissingKey(_)));
    }

    #[test]
    fn connect_with_key_no_secret_in_message() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let mut reg = ConnectRegistry::new();
        reg.register(opencode_go_profile());
        let mut ap = None;
        let mut am = None;
        // Offline unit test: skip live key verification. `FORGE_CONNECT_SKIP_VERIFY`
        // is read by `connect_api_key` in service.rs, so setting it unguarded used to
        // change whether *other* tests made real network calls.
        let guard = crate::test_env::EnvGuard::new(&[
            "OPENCODE_API_KEY",
            "OPENCODE_GO_API_KEY",
            "FORGE_CONNECT_SKIP_VERIFY",
        ]);
        guard.set("FORGE_CONNECT_SKIP_VERIFY", "1");
        let msg = handle_connect_action(
            ConnectAction::Connect {
                profile_id: "opencode_go".into(),
                // Long enough to pass the length gate when verify is skipped.
                api_key: Some("go-secret-key-for-tests".into()),
                oauth_fixture: false,
            },
            &reg,
            &store,
            &mut ap,
            &mut am,
        )
        .unwrap();
        assert!(msg.contains("OpenCode Go"));
        assert!(!msg.contains("go-secret-key"));
        assert_eq!(ap.as_deref(), Some("opencode_go"));
        assert!(am.as_deref().unwrap().starts_with("opencode-go/"));
    }

    #[test]
    fn short_key_rejected_without_network() {
        let err = verify_api_key("short", DEFAULT_BASE_URL)
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
    fn profile_uses_go_endpoint() {
        let p = opencode_go_profile();
        assert!(p
            .default_base_url
            .as_deref()
            .unwrap()
            .contains("/zen/go/v1"));
        assert!(!p
            .default_models
            .iter()
            .any(|m| m.starts_with("openrouter/")));
    }

    #[test]
    fn verify_accepts_success_and_reports_failures_without_secret() {
        assert!(verify_api_key("go-valid-key-for-tests", &mock_server(200)).is_ok());

        let err = verify_api_key("go-valid-key-for-tests", &mock_server(403))
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 403"), "{err}");
        assert!(!err.contains("go-valid"));

        let err = verify_api_key("go-valid-key-for-tests", &mock_server(503))
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 503"), "{err}");
        assert!(!err.contains("go-valid"));
    }
}
