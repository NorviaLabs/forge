mod anthropic_messages;
mod chat_completions;
mod protocol;
mod responses;

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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use forge_config::Config;
use forge_connect::{
    Attach, CredentialError, CredentialValue, Credentials, RouteTable, SessionHeaderKind,
    StaticHeader, WireKind,
};
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
    /// The routing snapshot. Dispatch clones the `Arc` and drops the guard
    /// before any await, so an in-flight turn keeps its snapshot.
    routes: RwLock<RouteTable>,
    image_cache: Mutex<HashMap<PathBuf, CachedImage>>,
}

/// The request's resolved destination (design §4): base URL, model ids, and
/// how the credential and fixed headers go on the request.
pub(super) struct Target {
    pub(super) profile_id: String,
    pub(super) base_url: String,
    /// `<prefix>/<bare>`, the id effort and catalog lookups key on.
    pub(super) model: String,
    /// The id sent upstream.
    pub(super) bare_model: String,
    attach: Attach,
    optional: bool,
    credential: Option<CredentialValue>,
    static_headers: Vec<StaticHeader>,
    session_header: Option<SessionHeaderKind>,
    definition: forge_connect::RouteDefinition,
}

/// User agent OpenCode's gateway expects with its session header.
const OPENCODE_USER_AGENT: &str = concat!("forge/", env!("CARGO_PKG_VERSION"));

/// Process-stable OpenCode session id for requests without a Forge session.
static FALLBACK_OPENCODE_SESSION: OnceLock<String> = OnceLock::new();

fn opencode_session_id(req: &ModelRequest) -> &str {
    req.session_id.as_deref().unwrap_or_else(|| {
        FALLBACK_OPENCODE_SESSION.get_or_init(|| uuid::Uuid::new_v4().to_string())
    })
}

impl Target {
    /// The effort levels this model takes (`forge_connect::effort`).
    pub(super) fn effort_support(&self) -> forge_connect::effort::EffortSupport {
        forge_connect::effort::effort(
            &self.definition,
            &self.bare_model,
            &forge_connect::ModelCatalogCache::user_default(),
        )
    }

    /// Add the profile's fixed headers, its session header, and the
    /// credential per its `attach`. A missing credential fails before
    /// sending unless the profile's key is optional.
    pub(super) fn authorize(
        &self,
        request: reqwest::RequestBuilder,
        req: &ModelRequest,
    ) -> Result<reqwest::RequestBuilder, ModelError> {
        let mut request = request;
        for header in &self.static_headers {
            request = request.header(header.name.as_str(), header.value.as_str());
        }
        if self.session_header == Some(SessionHeaderKind::Opencode) {
            request = request
                .header("User-Agent", OPENCODE_USER_AGENT)
                .header("x-opencode-session", opencode_session_id(req));
        }
        Ok(match (self.attach, &self.credential) {
            (Attach::None, _) => request,
            (Attach::Bearer, Some(value)) => request.bearer_auth(&value.secret),
            (Attach::XApiKey, Some(value)) => request.header("x-api-key", &value.secret),
            (Attach::BearerAccountId, Some(value)) => {
                let account = value
                    .account_id
                    .as_deref()
                    .ok_or(ModelError::MissingApiKey)?;
                request
                    .bearer_auth(&value.secret)
                    .header("chatgpt-account-id", account)
            }
            (_, None) if self.optional => request,
            (_, None) => return Err(ModelError::MissingApiKey),
        })
    }
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
        // Sessions replace this with their own snapshot; a bare client (bench,
        // one-off calls) routes through the registry and the process handle.
        Ok(Self {
            http,
            routes: RwLock::new(forge_connect::resolve(
                &forge_connect::loaded_registry(),
                &Credentials::shared(),
            )),
            image_cache: Mutex::new(HashMap::new()),
        })
    }

    fn model_for(&self, req: &ModelRequest) -> Result<String, ModelError> {
        let model = req.model.trim();
        if model.is_empty() {
            return Err(ModelError::Other("model id is required".into()));
        }
        Ok(model.to_string())
    }

    fn route_table(&self) -> RouteTable {
        self.routes
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set_routes(&self, table: RouteTable) {
        *self
            .routes
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = table;
    }

    /// The wire a request would use, for prompt projection and cache keys.
    fn wire_for_request(&self, req: &ModelRequest) -> WireKind {
        let Ok(model) = self.model_for(req) else {
            return WireKind::ChatCompletions;
        };
        self.route_table()
            .profile_for_request(&model)
            .map(|(found, bare)| found.definition.wire_for_model(bare))
            .unwrap_or(WireKind::ChatCompletions)
    }

    /// Pick the profile, refresh and read its credential, and build the
    /// request target (design §4).
    async fn target_for(&self, req: &ModelRequest) -> Result<(Target, WireKind), ModelError> {
        let model = self.model_for(req)?;
        let table = self.route_table();
        let (found, bare) = table
            .profile_for_request(&model)
            .ok_or_else(|| unknown_route(&table, &model))?;
        let definition = &found.definition;
        let credential = found.credential.clone();
        let refresh = credential.clone();
        tokio::task::spawn_blocking(move || refresh.ensure_fresh_blocking())
            .await
            .map_err(|error| ModelError::Other(error.to_string()))?
            .map_err(|error| credential_error(error, &definition.profile_id))?;
        let value = credential
            .current()
            .map_err(|error| credential_error(error, &definition.profile_id))?;
        let wire = definition.wire_for_model(bare);
        Ok((
            Target {
                profile_id: definition.profile_id.clone(),
                base_url: definition.base_url.clone(),
                model: format!("{}/{bare}", definition.model_provider_prefix),
                bare_model: bare.to_string(),
                attach: definition.attach,
                optional: definition.auth_kind == forge_connect::AuthKind::ApiKeyOptional,
                credential: value,
                static_headers: definition.static_headers.clone(),
                session_header: definition.session_header,
                definition: definition.clone(),
            },
            wire,
        ))
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

fn unknown_route(table: &RouteTable, model: &str) -> ModelError {
    // An id saved under an old prefix (`openai-codex/…`) names its new form.
    if let Some(current) = forge_connect::old_prefix_hint(model) {
        return ModelError::UnknownRoute(format!(
            "model `{model}` uses an old provider prefix; did you mean `{current}`?"
        ));
    }
    let known: Vec<&str> = table
        .profiles()
        .map(|found| found.definition.profile_id.as_str())
        .collect();
    ModelError::UnknownRoute(format!(
        "no provider serves model `{model}`; model ids are `<provider>/<model>` with one of: {}",
        known.join(", ")
    ))
}

fn credential_error(error: CredentialError, profile_id: &str) -> ModelError {
    match error {
        CredentialError::Missing => ModelError::MissingApiKey,
        CredentialError::AuthRequired => ModelError::ProviderAuth {
            status: 401,
            message: format!(
                "the stored credential for `{profile_id}` was rejected; reconnect with /connect {profile_id}"
            ),
        },
        CredentialError::Store(message) => {
            ModelError::Other(format!("credentials.toml ({profile_id}): {message}"))
        }
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
        let (target, wire) = self.target_for(&req).await?;
        protocol::complete(self, req, target, wire, tx).await
    }

    fn set_route_table(&self, table: RouteTable) {
        self.set_routes(table);
    }

    fn prompt_wire(&self, req: &ModelRequest) -> serde_json::Value {
        protocol::prompt_projection(self, req, self.wire_for_request(req))
    }

    fn prompt_transport_key(&self, req: &ModelRequest) -> &'static str {
        protocol::cache_key(self.wire_for_request(req))
    }
}

/// Turn a non-success provider response into a [`ModelError`], the same way on
/// every transport:
///
/// - a 3xx is refused (the client follows no redirects) and names its target
///   host;
/// - a 401 is a credential failure for the target's profile
///   ([`ModelError::ProviderAuth`]);
/// - a 403 is a refusal of this request for the model, not proof the
///   credential is bad, so it stays a [`ModelError::ProviderStatus`];
/// - anything else carries the provider's body as detail.
pub(super) async fn status_error(response: reqwest::Response, target: &Target) -> ModelError {
    let status = response.status();
    let code = status.as_u16();
    if status.is_redirection() {
        let location = response
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
                "redirect to {location} refused; Forge does not follow provider redirects"
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
            message: format!(
                "HTTP 401: authentication failed for `{}`; run /connect{suffix}",
                target.profile_id
            ),
        },
        403 => ModelError::ProviderStatus {
            status: code,
            detail: format!(
                "the provider refused this request for `{}`{suffix}",
                target.model
            ),
        },
        _ => ModelError::ProviderStatus {
            status: code,
            detail: detail.to_string(),
        },
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use forge_connect::{Credential, RouteDefinition};

    /// A client whose route table holds only `profile`, sent to `base_url`
    /// with a fixed credential.
    pub(crate) fn test_client(
        profile: forge_connect::ProviderSpec,
        base_url: &str,
        credential: Option<CredentialValue>,
    ) -> NativeModelClient {
        let client = NativeModelClient::from_config(&Config::default()).unwrap();
        let mut definition = RouteDefinition::from_spec(&profile);
        definition.base_url = base_url.trim_end_matches('/').to_string();
        client.set_routes(RouteTable::single(
            definition,
            Credential::Static(credential),
        ));
        client
    }

    pub(crate) fn key(secret: &str) -> Option<CredentialValue> {
        Some(CredentialValue {
            secret: secret.into(),
            account_id: None,
        })
    }

    /// A key with a ChatGPT account id, for the `bearer-account-id` attach.
    pub(crate) fn account_key(secret: &str) -> Option<CredentialValue> {
        Some(CredentialValue {
            secret: secret.into(),
            account_id: Some("account".into()),
        })
    }

    pub(crate) fn request(model: &str) -> ModelRequest {
        ModelRequest {
            messages: vec![forge_types::Message::new(
                forge_types::MessageRole::User,
                "hello",
            )]
            .into(),
            tools: Vec::new(),
            model: model.into(),
            workspace_root: PathBuf::new(),
            session_id: None,
            reasoning_effort: None,
            thinking_enabled: false,
            prompt_cache: false,
        }
    }

    /// A route table over the built-ins, reading credentials from a temp
    /// store.
    fn builtin_table(dir: &tempfile::TempDir) -> (RouteTable, Arc<Credentials>) {
        let creds = Arc::new(Credentials::new(forge_connect::CredentialStore::new(
            dir.path().join("credentials.toml"),
        )));
        let mut registry = forge_connect::builtin_registry();
        // Ignore ambient vendor env so the stored login is what resolves.
        for id in ["xai", "openai_codex"] {
            let mut spec = registry.get(id).unwrap().clone();
            spec.api_key_env.clear();
            registry.register(spec);
        }
        (forge_connect::resolve(&registry, &creds), creds)
    }

    /// A ChatGPT access token whose payload names account `account-123`.
    fn chatgpt_token() -> String {
        "header.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjb3VudC0xMjMifX0.signature".into()
    }

    fn oauth(access: &str) -> forge_connect::OauthTokens {
        forge_connect::OauthTokens {
            access_token: access.into(),
            refresh_token: None,
            expires_at: None,
        }
    }

    #[test]
    fn config_constructs_without_python() {
        NativeModelClient::from_config(&Config::default()).unwrap();
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

    #[tokio::test]
    async fn the_model_prefix_picks_the_profile_and_old_prefixes_get_a_hint() {
        let dir = tempfile::tempdir().unwrap();
        let (table, creds) = builtin_table(&dir);
        let codex = forge_connect::builtin::builtin_profile("openai_codex");
        creds.set_oauth(&codex, oauth(&chatgpt_token())).unwrap();
        let client = NativeModelClient::from_config(&Config::default()).unwrap();
        client.set_routes(table);

        // `bench --model openai_codex/gpt-5`
        let (target, wire) = client
            .target_for(&request("openai_codex/gpt-5"))
            .await
            .unwrap();
        assert_eq!(target.profile_id, "openai_codex");
        assert_eq!(target.bare_model, "gpt-5");
        assert_eq!(wire, WireKind::Responses);

        for (old, hint) in [
            ("openai-codex/gpt-5", "openai_codex/gpt-5"),
            ("grok/grok-4", "xai/grok-4"),
        ] {
            let error = client.target_for(&request(old)).await.err().unwrap();
            assert!(
                matches!(&error, ModelError::UnknownRoute(text) if text.contains(hint)),
                "{error}"
            );
        }
        let error = client
            .target_for(&request("nobody/model"))
            .await
            .err()
            .unwrap();
        assert!(matches!(error, ModelError::UnknownRoute(_)), "{error}");
    }

    #[tokio::test]
    async fn an_expiring_token_is_refreshed_before_the_next_request() {
        let Some(issuer) = forge_test_support::mock_http(vec![(
            200,
            r#"{"access_token":"refreshed","refresh_token":"new-rt","expires_in":3600}"#,
            vec![],
        )]) else {
            return;
        };
        let Some((chat, request_rx)) = super::test_support::serve_once(
            "200 OK",
            "text/event-stream",
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        )
        .await
        else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let creds = Arc::new(Credentials::new(forge_connect::CredentialStore::new(
            dir.path().join("credentials.toml"),
        )));
        let mut xai = forge_connect::builtin::builtin_profile("xai");
        xai.api_key_env.clear();
        xai.base_url = chat;
        // The test-only issuer override: refresh talks to the mock.
        if let Some(auth) = xai.auth.as_mut() {
            auth.issuer = Some(issuer.clone());
        }
        creds
            .set_oauth(
                &xai,
                forge_connect::OauthTokens {
                    access_token: "expired".into(),
                    refresh_token: Some("old-rt".into()),
                    expires_at: Some("0".into()),
                },
            )
            .unwrap();
        let mut registry = forge_connect::ConnectRegistry::new();
        registry.register(xai);
        let client = NativeModelClient::from_config(&Config::default()).unwrap();
        client.set_routes(forge_connect::resolve(&registry, &creds));

        let response = client.complete(request("xai/grok-4")).await;
        assert_eq!(response.unwrap().text, "hi");
        let sent = request_rx.await.unwrap().to_ascii_lowercase();
        assert!(sent.contains("authorization: bearer refreshed"), "{sent}");
    }

    #[tokio::test]
    async fn characterize_status_and_truncation_errors_per_transport() {
        use crate::ModelClient;
        let paths = [
            (
                "chat-completions",
                forge_connect::builtin::builtin_profile("openai"),
                "openai/m",
                "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            ),
            (
                "codex-responses",
                forge_connect::builtin::builtin_profile("openai_codex"),
                "openai_codex/gpt-5",
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
            ),
            (
                "anthropic-messages",
                forge_connect::builtin::builtin_profile("anthropic"),
                "anthropic/claude",
                concat!(
                    "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n",
                    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n"
                ),
            ),
        ];
        let mut actual = Vec::new();
        for (path, profile, model, truncated) in paths {
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
                let client = test_client(profile.clone(), &base, account_key("token"));
                let request = ModelRequest {
                    messages: vec![forge_types::Message::new(
                        forge_types::MessageRole::User,
                        "hello",
                    )]
                    .into(),
                    tools: Vec::new(),
                    model: model.into(),
                    workspace_root: PathBuf::new(),
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
    fn native_transport_keys_follow_the_profile_wire() {
        let client = NativeModelClient::from_config(&Config::default()).unwrap();
        let request = |model: &str| ModelRequest {
            messages: Default::default(),
            tools: Vec::new(),
            model: model.into(),
            workspace_root: PathBuf::new(),
            session_id: None,
            reasoning_effort: None,
            thinking_enabled: false,
            prompt_cache: false,
        };
        assert_eq!(
            client.prompt_transport_key(&request("anthropic/claude")),
            "anthropic_messages"
        );
        assert_eq!(
            client.prompt_transport_key(&request("openai_codex/gpt-5")),
            "codex"
        );
        assert_eq!(
            client.prompt_transport_key(&request("openai/gpt-5")),
            "openai_compat"
        );
        assert_eq!(
            client.prompt_transport_key(&request("model")),
            "openai_compat"
        );
    }
}
