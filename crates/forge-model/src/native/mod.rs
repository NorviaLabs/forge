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
        if model.starts_with("anthropic/") {
            return anthropic::complete(self, req, &model, tx).await;
        }
        if model.starts_with("openai-codex/") {
            return codex::complete(self, req, &model, tx).await;
        }
        openai::complete(self, req, &model, tx).await
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
}
