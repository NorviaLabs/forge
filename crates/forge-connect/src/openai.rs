//! OpenAI connect profile — API key (`openai/*`).

use crate::auth::AuthMode;
use crate::profile::ConnectProfile;

pub const PROFILE_ID: &str = "openai";
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

pub fn openai_profile() -> ConnectProfile {
    ConnectProfile {
        id: PROFILE_ID.into(),
        title: "OpenAI".into(),
        description: "OpenAI API key — openai/* models".into(),
        auth_mode: AuthMode::ApiKey {
            tui_always_prompt: true,
        },
        api_key_env: vec!["OPENAI_API_KEY".into()],
        default_base_url: Some(DEFAULT_BASE_URL.into()),
        default_models: vec!["openai/gpt-4.1-mini".into()],
        models_dev_providers: vec!["openai".into()],
        auth_url: Some("https://platform.openai.com/api-keys".into()),
        model_provider_prefix: "openai".into(),
    }
}

/// Verify key with `GET /models` (Bearer). Never includes the key in errors.
pub fn verify_api_key(api_key: &str, base_url: &str) -> Result<(), String> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err("API key is empty".into());
    }
    if key.len() < 16 {
        return Err(format!(
            "API key looks too short ({n} chars). Create a key at https://platform.openai.com/api-keys.",
            n = key.len()
        ));
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
            ureq::Error::Status(code, _) => format!(
                "OpenAI rejected the API key (HTTP {code}). \
Check the key at https://platform.openai.com/api-keys."
            ),
            other => format!("Could not reach OpenAI to verify key ({other}). Check network."),
        })?;
    let status = resp.status();
    if (200..300).contains(&status) {
        Ok(())
    } else if status == 401 || status == 403 {
        Err("OpenAI rejected the API key (unauthorized). \
Create a key at https://platform.openai.com/api-keys."
            .into())
    } else {
        Err(format!("OpenAI key verification failed (HTTP {status})."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

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
    fn profile_shape() {
        let p = openai_profile();
        assert_eq!(p.id, "openai");
        assert!(p.needs_tui_api_key_prompt());
        assert!(p.default_models.iter().all(|m| m.starts_with("openai/")));
    }

    #[test]
    fn short_key_rejected() {
        let err = verify_api_key("sk-short", DEFAULT_BASE_URL).unwrap_err();
        assert!(err.contains("too short"), "{err}");
    }

    #[test]
    fn empty_key_rejected_without_network() {
        let err = verify_api_key("   ", DEFAULT_BASE_URL).unwrap_err();
        assert_eq!(err, "API key is empty");
    }

    #[test]
    fn verifies_success_with_trimmed_base_url() {
        let base = mock_server(200);
        assert!(verify_api_key("sk-valid-key-for-tests", &base).is_ok());
    }

    #[test]
    fn verify_reports_auth_and_server_statuses_without_secret() {
        let err = verify_api_key("sk-valid-key-for-tests", &mock_server(401)).unwrap_err();
        assert!(err.contains("HTTP 401"), "{err}");
        assert!(!err.contains("sk-valid"));

        let err = verify_api_key("sk-valid-key-for-tests", &mock_server(500)).unwrap_err();
        assert!(err.contains("HTTP 500"), "{err}");
        assert!(!err.contains("sk-valid"));
    }
}
