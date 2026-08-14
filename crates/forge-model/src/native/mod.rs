mod anthropic;
mod codex;
mod openai;

#[cfg(test)]
mod test_support {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    pub async fn serve_once(
        status: &str,
        content_type: &str,
        body: &str,
    ) -> (String, oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_string();
        let content_type = content_type.to_string();
        let body = body.to_string();
        let (request_tx, request_rx) = oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 4096];
            let header_end = loop {
                let count = socket.read(&mut chunk).await.unwrap();
                if count == 0 {
                    panic!("client closed before sending headers");
                }
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    break position + 4;
                }
            };
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while bytes.len() < header_end + content_length {
                let count = socket.read(&mut chunk).await.unwrap();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            let _ = request_tx.send(String::from_utf8_lossy(&bytes).into_owned());
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        (format!("http://{address}"), request_rx)
    }
}

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use forge_config::Config;
use forge_types::ModelResponse;

use crate::{ModelClient, ModelError, ModelRequest, StreamEventTx};

pub(super) fn process_sse_lines(
    pending: &mut String,
    mut consume: impl FnMut(&str) -> Result<(), ModelError>,
) -> Result<(), ModelError> {
    let mut start = 0;
    while let Some(relative) = pending[start..].find('\n') {
        let newline = start + relative;
        consume(pending[start..newline].trim())?;
        start = newline + 1;
    }
    if start != 0 {
        pending.drain(..start);
    }
    Ok(())
}

#[cfg(test)]
mod sse_tests {
    use super::process_sse_lines;

    #[test]
    fn keeps_partial_line_after_batch() {
        let mut pending = "data: one\ndata: two\ndata: par".into();
        let mut lines = Vec::new();
        process_sse_lines(&mut pending, |line| {
            lines.push(line.to_string());
            Ok(())
        })
        .unwrap();
        assert_eq!(lines, ["data: one", "data: two"]);
        assert_eq!(pending, "data: par");
    }
}

pub struct NativeModelClient {
    http: reqwest::Client,
    default_model: String,
    configured_base_url: Option<String>,
    configured_api_key: Option<String>,
    credentials: Arc<Mutex<BTreeMap<String, String>>>,
}

impl NativeModelClient {
    pub fn from_config(cfg: &Config) -> Result<Self, ModelError> {
        let timeout = Duration::from_secs(cfg.model.request_timeout_secs.max(1));
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| ModelError::Http(error.to_string()))?;
        Ok(Self {
            http,
            default_model: cfg.model.model.clone(),
            configured_base_url: cfg.model.base_url.clone(),
            configured_api_key: cfg.model.api_key.clone(),
            credentials: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    fn model_for(&self, req: &ModelRequest) -> Result<String, ModelError> {
        let model = if req.model.trim().is_empty() {
            self.default_model.trim()
        } else {
            req.model.trim()
        };
        if model.is_empty() {
            return Err(ModelError::Other("model id is required".into()));
        }
        Ok(model.to_string())
    }

    fn credential(&self, names: &[&str]) -> Option<String> {
        self.injected_or_env(names)
            .or_else(|| self.configured_api_key.clone())
    }

    fn injected_or_env(&self, names: &[&str]) -> Option<String> {
        let injected = self.credentials.lock().ok();
        names.iter().find_map(|name| {
            injected
                .as_ref()
                .and_then(|values| values.get(*name).cloned())
                .or_else(|| std::env::var(name).ok())
                .filter(|value| !value.trim().is_empty())
        })
    }

    pub(super) fn resolved_base_url(&self, env_names: &[&str], default: &str) -> String {
        self.configured_base_url
            .clone()
            .or_else(|| self.injected_or_env(env_names))
            .unwrap_or_else(|| default.into())
    }
}

#[async_trait]
impl ModelClient for NativeModelClient {
    async fn complete(&self, req: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.complete_with_stream(req, None).await
    }

    async fn complete_with_stream(
        &self,
        req: ModelRequest,
        tx: Option<StreamEventTx>,
    ) -> Result<ModelResponse, ModelError> {
        let model = self.model_for(&req)?;
        let model = canonical_model_for_route(req.route_id.as_deref(), &model);
        match transport_for_route(req.route_id.as_deref()) {
            forge_connect::ProviderTransport::Anthropic => {
                anthropic::complete(self, req, &model, tx).await
            }
            forge_connect::ProviderTransport::Codex => codex::complete(self, req, &model, tx).await,
            forge_connect::ProviderTransport::OpenaiCompat => {
                openai::complete(self, req, &model, tx).await
            }
        }
    }

    fn apply_provider_env(&self, pairs: &[(String, String)]) {
        if let Ok(mut credentials) = self.credentials.lock() {
            credentials.extend(pairs.iter().cloned());
        }
    }

    fn clear_provider_env(&self) {
        if let Ok(mut credentials) = self.credentials.lock() {
            credentials.clear();
        }
    }
}

/// Convert the stable offering identity into the namespaced model id the
/// wire modules consume. Prefix comes from the registered spec, not a match.
fn spec_for_route(route_id: &str) -> Option<forge_connect::ProviderSpec> {
    forge_connect::loaded_registry()
        .get_by_route(route_id)
        .cloned()
}

fn transport_for_route(route_id: Option<&str>) -> forge_connect::ProviderTransport {
    route_id
        .and_then(spec_for_route)
        .map(|spec| spec.transport)
        .unwrap_or(forge_connect::ProviderTransport::OpenaiCompat)
}

fn canonical_model_for_route(route_id: Option<&str>, model: &str) -> String {
    let Some(route_id) = route_id else {
        return model.to_string();
    };
    let Some(spec) = spec_for_route(route_id) else {
        return model.to_string();
    };
    let bare = model
        .split_once('/')
        .map(|(_, value)| value)
        .unwrap_or(model);
    format!("{}/{bare}", spec.model_provider_prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_constructs_without_python() {
        let client = NativeModelClient::from_config(&Config::default()).unwrap();
        assert!(client.credentials.lock().unwrap().is_empty());
    }

    #[test]
    fn injected_credentials_are_available() {
        let client = NativeModelClient::from_config(&Config::default()).unwrap();
        client.apply_provider_env(&[("OPENAI_API_KEY".into(), "injected".into())]);
        assert_eq!(
            client.credential(&["OPENAI_API_KEY"]).as_deref(),
            Some("injected")
        );
    }

    #[test]
    fn stable_route_identity_controls_legacy_wire_namespace() {
        assert_eq!(
            canonical_model_for_route(Some("openai-chatgpt"), "gpt-5.6"),
            "openai-codex/gpt-5.6"
        );
        assert_eq!(
            canonical_model_for_route(Some("openai-api"), "gpt-5.6"),
            "openai/gpt-5.6"
        );
        assert_eq!(
            canonical_model_for_route(Some("anthropic-api"), "claude-sonnet-4-5"),
            "anthropic/claude-sonnet-4-5"
        );
        assert_eq!(
            canonical_model_for_route(Some("openai-api"), "vendor/model"),
            "openai/model"
        );
        assert_eq!(
            canonical_model_for_route(None, "vendor/model"),
            "vendor/model"
        );
    }
}
