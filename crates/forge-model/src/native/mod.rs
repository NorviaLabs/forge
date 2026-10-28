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
    ) -> Option<(String, oneshot::Receiver<String>)> {
        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping: this host denies binding a mock listener");
                return None;
            }
            Err(e) => panic!("bind mock listener: {e}"),
        };
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
        Some((format!("http://{address}"), request_rx))
    }
}

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use forge_config::Config;
use forge_types::{ImageRef, ModelResponse};

use crate::image::{load_image_ref, LoadedImage};
use crate::{ModelClient, ModelError, ModelRequest, StreamEventTx};

const IMAGE_CACHE_MAX_ENTRIES: usize = 16;

struct CachedImage {
    modified: Option<std::time::SystemTime>,
    byte_len: u64,
    image: Arc<LoadedImage>,
}

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
    image_cache: Mutex<HashMap<PathBuf, CachedImage>>,
}

impl NativeModelClient {
    pub fn from_config(cfg: &Config) -> Result<Self, ModelError> {
        let timeout = Duration::from_secs(cfg.model.request_timeout_secs.max(1));
        // Never follow redirects: reqwest strips `Authorization` on a cross-host
        // hop but forwards `x-api-key`, `chatgpt-account-id` and, on 307/308,
        // the prompt body. A redirect surfaces as an error naming its target.
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| ModelError::Http(error.to_string()))?;
        Ok(Self {
            http,
            default_model: cfg.model.model.clone(),
            configured_base_url: cfg.model.base_url.clone(),
            configured_api_key: cfg.model.api_key.clone(),
            credentials: Arc::new(Mutex::new(BTreeMap::new())),
            image_cache: Mutex::new(HashMap::new()),
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

    pub(super) fn load_image_ref(
        &self,
        workspace: &Path,
        image: &ImageRef,
    ) -> Result<Arc<LoadedImage>, String> {
        let path = crate::image::resolve_image_path(workspace, &image.path);
        let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
        let modified = metadata.modified().ok();
        if let Ok(cache) = self.image_cache.lock() {
            if let Some(cached) = cache.get(&path) {
                if cached.byte_len == metadata.len() && cached.modified == modified {
                    return Ok(Arc::clone(&cached.image));
                }
            }
        }

        let loaded = Arc::new(load_image_ref(workspace, image)?);
        if let Ok(mut cache) = self.image_cache.lock() {
            if cache.len() >= IMAGE_CACHE_MAX_ENTRIES {
                cache.clear();
            }
            cache.insert(
                path,
                CachedImage {
                    modified,
                    byte_len: metadata.len(),
                    image: Arc::clone(&loaded),
                },
            );
        }
        Ok(loaded)
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

    fn prompt_wire(&self, req: &ModelRequest) -> serde_json::Value {
        match transport_for_route(req.route_id.as_deref()) {
            forge_connect::ProviderTransport::Anthropic => {
                let (system, messages) = anthropic::messages_body(req, Some(self));
                let mut body = serde_json::json!({ "messages": messages });
                if !system.is_empty() {
                    body["system"] = serde_json::Value::String(system);
                }
                if !req.tools.is_empty() {
                    body["tools"] = serde_json::Value::Array(
                        req.tools
                            .iter()
                            .map(|tool| {
                                serde_json::json!({
                                    "name": tool.name,
                                    "description": tool.description,
                                    "input_schema": tool.input_schema
                                })
                            })
                            .collect(),
                    );
                }
                if req.prompt_cache {
                    crate::prompt_cache::apply_anthropic_prompt_cache(&mut body);
                }
                crate::prompt_wire::prompt_object_from_body(body)
            }
            forge_connect::ProviderTransport::Codex => crate::prompt_wire::prompt_object_from_body(
                codex::request_body(self, req, &req.model, &codex::tool_aliases(req)),
            ),
            forge_connect::ProviderTransport::OpenaiCompat => {
                crate::prompt_wire::openai_compat_prompt_cached(req, self)
            }
        }
    }

    fn prompt_transport_key(&self, req: &ModelRequest) -> &'static str {
        match transport_for_route(req.route_id.as_deref()) {
            forge_connect::ProviderTransport::Anthropic => "anthropic",
            forge_connect::ProviderTransport::Codex => "codex",
            forge_connect::ProviderTransport::OpenaiCompat => "openai_compat",
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

/// Turn a non-success provider response into a [`ModelError`], the same way on
/// every transport:
///
/// - a 3xx is refused (the client follows no redirects) and names its target
///   host;
/// - a 401 is a credential failure ([`ModelError::ProviderAuth`]);
/// - a 403 is a refusal of this request for `model`, not proof the credential
///   is bad, so it stays a [`ModelError::ProviderStatus`];
/// - anything else carries the provider's body as detail.
pub(super) async fn status_error(response: reqwest::Response, model: &str) -> ModelError {
    let status = response.status();
    let code = status.as_u16();
    if status.is_redirection() {
        let target = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(|location| {
                reqwest::Url::parse(location)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_string))
                    .unwrap_or_else(|| location.to_string())
            })
            .unwrap_or_else(|| "an unspecified location".to_string());
        return ModelError::ProviderStatus {
            status: code,
            detail: format!(
                "redirect to {target} refused; Forge does not follow provider redirects"
            ),
        };
    }
    let detail = response.text().await.unwrap_or_default();
    let detail = detail.trim();
    let suffix = if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    };
    match code {
        401 => ModelError::ProviderAuth {
            status: code,
            message: format!("HTTP 401: authentication failed; run /connect{suffix}"),
        },
        403 => ModelError::ProviderStatus {
            status: code,
            detail: format!("the provider refused this request for `{model}`{suffix}"),
        },
        _ => ModelError::ProviderStatus {
            status: code,
            detail: detail.to_string(),
        },
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

    #[test]
    fn image_requests_reuse_cached_bytes_until_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        std::fs::write(&path, forge_types::sample_png_bytes()).unwrap();
        let image = forge_types::ImageRef::new("shot.png", "image/png", 1);
        let client = NativeModelClient::from_config(&Config::default()).unwrap();

        let first = client.load_image_ref(dir.path(), &image).unwrap();
        let encoded = first.base64().as_ptr();
        let second = client.load_image_ref(dir.path(), &image).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(encoded, second.base64().as_ptr());

        let mut changed = forge_types::sample_png_bytes();
        changed.push(0);
        std::fs::write(&path, changed).unwrap();
        let third = client.load_image_ref(dir.path(), &image).unwrap();
        assert!(!Arc::ptr_eq(&second, &third));
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

    #[test]
    fn native_request_and_credential_fallbacks_are_deterministic() {
        let mut cfg = Config::default();
        cfg.model.model = "configured-model".into();
        cfg.model.base_url = Some("https://configured.test".into());
        cfg.model.api_key = Some("configured-key".into());
        let client = NativeModelClient::from_config(&cfg).unwrap();

        let blank = ModelRequest {
            messages: Default::default(),
            tools: Vec::new(),
            model: "  ".into(),
            workspace_root: PathBuf::new(),
            route_id: None,
            session_id: None,
            reasoning_effort: None,
            thinking_enabled: false,
            prompt_cache: false,
        };
        assert_eq!(client.model_for(&blank).unwrap(), "configured-model");
        assert_eq!(
            client.credential(&["MISSING_KEY"]).as_deref(),
            Some("configured-key")
        );
        assert_eq!(
            client.resolved_base_url(&["MISSING_URL"], "https://default.test"),
            "https://configured.test"
        );

        let mut empty = cfg;
        empty.model.model.clear();
        empty.model.api_key = None;
        empty.model.base_url = None;
        let empty_client = NativeModelClient::from_config(&empty).unwrap();
        assert!(matches!(
            empty_client.model_for(&blank),
            Err(ModelError::Other(_))
        ));
        assert_eq!(
            empty_client.resolved_base_url(&["MISSING_URL"], "https://default.test"),
            "https://default.test"
        );

        client.apply_provider_env(&[("OPENAI_API_KEY".into(), "override".into())]);
        assert_eq!(
            client.credential(&["OPENAI_API_KEY"]).as_deref(),
            Some("override")
        );
        client.clear_provider_env();
        assert_eq!(
            client.credential(&["OPENAI_API_KEY"]).as_deref(),
            Some("configured-key")
        );
    }

    /// Every credential and base URL variable a built-in route reads.
    const ROUTE_ENV: &[&str] = &[
        "OPENAI_API_KEY",
        "OPENAI_API_BASE",
        "XAI_API_KEY",
        "GROK_CODE_XAI_API_KEY",
        "XAI_API_BASE",
        "OPENCODE_API_KEY",
        "OPENCODE_GO_API_KEY",
        "OPENCODE_ZEN_API_KEY",
        "OPENCODE_API_BASE",
        "OPENCODE_GO_API_BASE",
        "OPENCODE_ZEN_API_BASE",
        "OLLAMA_API_KEY",
        "OLLAMA_API_BASE",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_API_BASE",
        "FORGE_CODEX_ACCESS_TOKEN",
        "FORGE_CODEX_ACCOUNT_ID",
        "FORGE_CODEX_API_BASE",
    ];

    /// `base_url key=<credential>` the way each transport resolves a model.
    fn resolved_route(client: &NativeModelClient, model: &str) -> String {
        if model.starts_with("anthropic/") {
            let (base, key) = anthropic::base_url_and_key(client);
            return format!("{base} key={}", key.as_deref().unwrap_or("-"));
        }
        if model.starts_with("openai-codex/") {
            let (token, account) = codex::session_credentials(client);
            return format!(
                "{} key={} account={}",
                codex::codex_responses_url(client),
                token.as_deref().unwrap_or("-"),
                account.as_deref().unwrap_or("-")
            );
        }
        match openai::route(client, model) {
            Ok(route) => format!(
                "{} key={}",
                route.base_url,
                route.api_key.as_deref().unwrap_or("-")
            ),
            Err(error) => format!("{error:?}"),
        }
    }

    /// Characterization (design stage 1): base URL and credential source per
    /// route prefix with no env, injected pairs, `cfg.model.base_url` and
    /// `api_key`, and both. Injecting an empty value masks the ambient env, so
    /// the "no env" case holds on any machine. Injected pairs and process env
    /// share one lookup (`injected_or_env`, injected first), which
    /// `injected_pairs_shadow_process_env` pins with a test-only variable.
    #[test]
    fn characterize_route_base_url_and_credential_sources() {
        let masked: Vec<(String, String)> = ROUTE_ENV
            .iter()
            .map(|name| (name.to_string(), String::new()))
            .collect();
        let injected: Vec<(String, String)> = ROUTE_ENV
            .iter()
            .map(|name| (name.to_string(), format!("${name}")))
            .collect();
        let mut configured = Config::default();
        configured.model.base_url = Some("https://configured.test".into());
        configured.model.api_key = Some("configured-key".into());
        let cases = [
            ("none", Config::default(), &masked),
            ("injected", Config::default(), &injected),
            ("config", configured.clone(), &masked),
            ("config+injected", configured, &injected),
        ];
        let models = [
            "openai/m",
            "xai/m",
            "grok/m",
            "opencode-go/m",
            "opencode/m",
            "opencode-zen/m",
            "ollama/m",
            "ollama_chat/m",
            "anthropic/m",
            "openai-codex/m",
        ];
        let mut actual = Vec::new();
        for (case, cfg, env) in cases {
            let client = NativeModelClient::from_config(&cfg).unwrap();
            client.apply_provider_env(env);
            for model in models {
                actual.push(format!(
                    "{case} {model} -> {}",
                    resolved_route(&client, model)
                ));
            }
        }
        // Today `cfg.model.api_key` also fills the ChatGPT account id, and
        // `cfg.model.base_url` overrides every route except OpenCode's.
        let expected = [
            "none openai/m -> MissingApiKey",
            "none xai/m -> MissingApiKey",
            "none grok/m -> MissingApiKey",
            "none opencode-go/m -> MissingApiKey",
            "none opencode/m -> MissingApiKey",
            "none opencode-zen/m -> MissingApiKey",
            "none ollama/m -> http://localhost:11434/v1 key=-",
            "none ollama_chat/m -> http://localhost:11434/v1 key=-",
            "none anthropic/m -> https://api.anthropic.com key=-",
            "none openai-codex/m -> https://chatgpt.com/backend-api/codex/responses key=- account=-",
            "injected openai/m -> $OPENAI_API_BASE key=$OPENAI_API_KEY",
            "injected xai/m -> $XAI_API_BASE key=$XAI_API_KEY",
            "injected grok/m -> $XAI_API_BASE key=$XAI_API_KEY",
            "injected opencode-go/m -> $OPENCODE_API_BASE key=$OPENCODE_API_KEY",
            "injected opencode/m -> $OPENCODE_API_BASE key=$OPENCODE_API_KEY",
            "injected opencode-zen/m -> $OPENCODE_ZEN_API_BASE key=$OPENCODE_ZEN_API_KEY",
            "injected ollama/m -> $OLLAMA_API_BASE/v1 key=$OLLAMA_API_KEY",
            "injected ollama_chat/m -> $OLLAMA_API_BASE/v1 key=$OLLAMA_API_KEY",
            "injected anthropic/m -> $ANTHROPIC_API_BASE key=$ANTHROPIC_API_KEY",
            "injected openai-codex/m -> $FORGE_CODEX_API_BASE/codex/responses key=$FORGE_CODEX_ACCESS_TOKEN account=$FORGE_CODEX_ACCOUNT_ID",
            "config openai/m -> https://configured.test key=configured-key",
            "config xai/m -> https://configured.test key=configured-key",
            "config grok/m -> https://configured.test key=configured-key",
            "config opencode-go/m -> https://opencode.ai/zen/go/v1 key=configured-key",
            "config opencode/m -> https://opencode.ai/zen/go/v1 key=configured-key",
            "config opencode-zen/m -> https://opencode.ai/zen/v1 key=configured-key",
            "config ollama/m -> https://configured.test/v1 key=configured-key",
            "config ollama_chat/m -> https://configured.test/v1 key=configured-key",
            "config anthropic/m -> https://configured.test key=configured-key",
            "config openai-codex/m -> https://configured.test/codex/responses key=configured-key account=configured-key",
            "config+injected openai/m -> https://configured.test key=$OPENAI_API_KEY",
            "config+injected xai/m -> https://configured.test key=$XAI_API_KEY",
            "config+injected grok/m -> https://configured.test key=$XAI_API_KEY",
            "config+injected opencode-go/m -> $OPENCODE_API_BASE key=$OPENCODE_API_KEY",
            "config+injected opencode/m -> $OPENCODE_API_BASE key=$OPENCODE_API_KEY",
            "config+injected opencode-zen/m -> $OPENCODE_ZEN_API_BASE key=$OPENCODE_ZEN_API_KEY",
            "config+injected ollama/m -> $OLLAMA_API_BASE/v1 key=$OLLAMA_API_KEY",
            "config+injected ollama_chat/m -> $OLLAMA_API_BASE/v1 key=$OLLAMA_API_KEY",
            "config+injected anthropic/m -> https://configured.test key=$ANTHROPIC_API_KEY",
            "config+injected openai-codex/m -> https://configured.test/codex/responses key=$FORGE_CODEX_ACCESS_TOKEN account=$FORGE_CODEX_ACCOUNT_ID",
        ];
        assert_eq!(actual, expected);
    }

    /// Characterization (design stage 1): the error each transport returns
    /// for 401, 403 and 500, and for a stream that ends without its
    /// completion event.
    #[tokio::test]
    async fn characterize_status_and_truncation_errors_per_transport() {
        use crate::ModelClient;
        let paths = [
            (
                "chat-completions",
                None,
                "openai/m",
                "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            ),
            (
                "codex-responses",
                Some("openai-chatgpt"),
                "gpt-5",
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
            ),
            (
                "anthropic-messages",
                Some("anthropic-api"),
                "anthropic/claude",
                concat!(
                    "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n",
                    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n"
                ),
            ),
        ];
        let mut actual = Vec::new();
        for (path, route_id, model, truncated) in paths {
            for (case, status, body, content_type) in [
                ("401", 401, "{}", "application/json"),
                ("403", 403, "{}", "application/json"),
                ("500", 500, "{}", "application/json"),
                ("truncated", 200, truncated, "text/event-stream"),
            ] {
                let Some(base) = forge_test_support::mock_http(vec![(
                    status,
                    body,
                    vec![("content-type", content_type)],
                )]) else {
                    return;
                };
                let mut config = Config::default();
                config.model.base_url = Some(base);
                let client = NativeModelClient::from_config(&config).unwrap();
                client.apply_provider_env(&[
                    ("OPENAI_API_KEY".into(), "key".into()),
                    ("ANTHROPIC_API_KEY".into(), "key".into()),
                    ("FORGE_CODEX_ACCESS_TOKEN".into(), "token".into()),
                    ("FORGE_CODEX_ACCOUNT_ID".into(), "account".into()),
                ]);
                let request = ModelRequest {
                    messages: vec![forge_types::Message::new(
                        forge_types::MessageRole::User,
                        "hello",
                    )]
                    .into(),
                    tools: Vec::new(),
                    model: model.into(),
                    workspace_root: PathBuf::new(),
                    route_id: route_id.map(str::to_owned),
                    session_id: None,
                    reasoning_effort: None,
                    thinking_enabled: false,
                    prompt_cache: false,
                };
                let outcome = match client.complete(request).await {
                    Ok(_) => "Ok".to_string(),
                    Err(error) => {
                        let debug = format!("{error:?}");
                        let variant = debug
                            .split(|c: char| !c.is_alphanumeric())
                            .next()
                            .unwrap_or_default()
                            .to_string();
                        match error.status() {
                            Some(status) => format!("{variant} {status}"),
                            None => variant,
                        }
                    }
                };
                actual.push(format!("{path} {case} -> {outcome}"));
            }
        }
        let expected: Vec<String> = ["chat-completions", "codex-responses", "anthropic-messages"]
            .iter()
            .flat_map(|path| {
                [
                    format!("{path} 401 -> ProviderAuth 401"),
                    format!("{path} 403 -> ProviderStatus 403"),
                    format!("{path} 500 -> ProviderStatus 500"),
                    format!("{path} truncated -> Transport"),
                ]
            })
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn injected_pairs_shadow_process_env() {
        const NAME: &str = "FORGE_MODEL_CHARACTERIZATION_ONLY_KEY";
        std::env::set_var(NAME, "from-env");
        let client = NativeModelClient::from_config(&Config::default()).unwrap();
        assert_eq!(client.credential(&[NAME]).as_deref(), Some("from-env"));
        assert_eq!(client.resolved_base_url(&[NAME], "default"), "from-env");
        client.apply_provider_env(&[(NAME.into(), "injected".into())]);
        assert_eq!(client.credential(&[NAME]).as_deref(), Some("injected"));
        client.apply_provider_env(&[(NAME.into(), String::new())]);
        assert_eq!(client.credential(&[NAME]), None);
        std::env::remove_var(NAME);
    }

    #[test]
    fn native_transport_keys_follow_route_kind() {
        let client = NativeModelClient::from_config(&Config::default()).unwrap();
        let request = |route_id: Option<&str>| ModelRequest {
            route_id: route_id.map(str::to_owned),
            messages: Default::default(),
            tools: Vec::new(),
            model: "model".into(),
            workspace_root: PathBuf::new(),
            session_id: None,
            reasoning_effort: None,
            thinking_enabled: false,
            prompt_cache: false,
        };
        assert_eq!(
            client.prompt_transport_key(&request(Some("anthropic-api"))),
            "anthropic"
        );
        assert_eq!(
            client.prompt_transport_key(&request(Some("openai-chatgpt"))),
            "codex"
        );
        assert_eq!(
            client.prompt_transport_key(&request(Some("openai-api"))),
            "openai_compat"
        );
        assert_eq!(client.prompt_transport_key(&request(None)), "openai_compat");
    }
}
