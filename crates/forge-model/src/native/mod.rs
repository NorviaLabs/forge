mod anthropic;
mod codex;
mod openai;

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
