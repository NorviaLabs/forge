//! Provider specs: identity, auth, catalog source, and transport.

use serde::{Deserialize, Serialize};

use crate::auth::AuthMode;

/// Where a spec's model list and capability metadata come from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CatalogMode {
    /// Live provider `/models` entitles; models.dev supplies effort/image/cost.
    #[serde(rename = "live+registry")]
    #[default]
    LiveRegistry,
    /// models.dev metadata only. Never entitlement.
    Registry,
    /// Live provider `/models` only.
    Live,
    /// `default_models` on the spec. No metadata.
    Static,
}

/// Wire implementation a request is sent through.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderTransport {
    #[default]
    #[serde(rename = "openai-compat")]
    OpenaiCompat,
    Anthropic,
    Codex,
}

/// How requests are encoded and responses parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WireKind {
    ChatCompletions,
    Responses,
    AnthropicMessages,
}

impl From<ProviderTransport> for WireKind {
    fn from(transport: ProviderTransport) -> Self {
        match transport {
            ProviderTransport::OpenaiCompat => Self::ChatCompletions,
            ProviderTransport::Anthropic => Self::AnthropicMessages,
            ProviderTransport::Codex => Self::Responses,
        }
    }
}

impl From<WireKind> for ProviderTransport {
    fn from(wire: WireKind) -> Self {
        match wire {
            WireKind::ChatCompletions => Self::OpenaiCompat,
            WireKind::AnthropicMessages => Self::Anthropic,
            WireKind::Responses => Self::Codex,
        }
    }
}

/// How the user obtains a credential.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthKind {
    #[default]
    ApiKey,
    ApiKeyOptional,
    OauthDevice,
}

/// A Rust implementation of one device-login flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OauthFlavor {
    Rfc8628,
    Chatgpt,
}

/// When the TUI asks for a key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthPrompt {
    #[default]
    Tui,
    Browser,
    Never,
}

impl AuthPrompt {
    fn is_tui(&self) -> bool {
        *self == Self::Tui
    }
}

/// The `auth` table: how a profile's credential is obtained.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSpec {
    #[serde(default)]
    pub kind: AuthKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flavor: Option<OauthFlavor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "AuthPrompt::is_tui")]
    pub prompt: AuthPrompt,
}

impl AuthSpec {
    pub fn is_oauth(&self) -> bool {
        self.kind == AuthKind::OauthDevice
    }

    /// The legacy [`AuthMode`] this spec maps to, for readers not yet moved.
    pub fn to_auth_mode(&self) -> AuthMode {
        match self.kind {
            AuthKind::OauthDevice => AuthMode::Oauth {
                device_code: true,
                system_browser: self.prompt == AuthPrompt::Browser,
                // The `chatgpt` flavor owns its issuer.
                auth_server: match (self.flavor, &self.issuer) {
                    (_, Some(issuer)) => issuer.clone(),
                    (Some(OauthFlavor::Chatgpt), None) => {
                        crate::oauth_chatgpt::DEFAULT_AUTH_BASE.into()
                    }
                    (_, None) => String::new(),
                },
            },
            AuthKind::ApiKey => AuthMode::ApiKey {
                tui_always_prompt: self.prompt == AuthPrompt::Tui,
            },
            AuthKind::ApiKeyOptional => AuthMode::ApiKey {
                tui_always_prompt: false,
            },
        }
    }
}

impl From<&AuthMode> for AuthSpec {
    fn from(mode: &AuthMode) -> Self {
        match mode {
            AuthMode::Oauth {
                system_browser,
                auth_server,
                ..
            } => Self {
                kind: AuthKind::OauthDevice,
                issuer: Some(auth_server.clone()),
                prompt: if *system_browser {
                    AuthPrompt::Browser
                } else {
                    AuthPrompt::Tui
                },
                ..Self::default()
            },
            AuthMode::ApiKey { tui_always_prompt } => Self {
                kind: AuthKind::ApiKey,
                prompt: if *tui_always_prompt {
                    AuthPrompt::Tui
                } else {
                    AuthPrompt::Never
                },
                ..Self::default()
            },
        }
    }
}

/// A built-in field the user may override in `[provider_overrides.<id>]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditableField {
    BaseUrl,
}

/// A per-model protocol choice inside a profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireOverride {
    /// Bare model ids, or a prefix ending in `*`.
    pub models: Vec<String>,
    pub wire: WireKind,
}

impl WireOverride {
    pub fn matches(&self, bare_model: &str) -> bool {
        self.models
            .iter()
            .any(|pattern| match pattern.strip_suffix('*') {
                Some(prefix) => bare_model.starts_with(prefix),
                None => bare_model == pattern,
            })
    }
}

/// How the credential goes on the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Attach {
    Bearer,
    XApiKey,
    BearerAccountId,
    None,
}

/// A fixed, non-secret request header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticHeader {
    pub name: String,
    pub value: String,
}

/// A per-request session header family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionHeaderKind {
    Opencode,
}

/// Where the live model list comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CatalogKind {
    OpenaiModels,
    AnthropicModels,
    OllamaTags,
    ChatgptModels,
    Static,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CatalogFilter {
    /// OpenAI's chat-model filter.
    Chat,
}

/// The `catalog` table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<CatalogKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<CatalogFilter>,
}

impl CatalogSpec {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerifyKind {
    /// The catalog kind's probe, including its fallback.
    #[default]
    ModelsProbe,
    None,
}

/// The `verify` table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifySpec {
    #[serde(default)]
    pub kind: VerifyKind,
}

impl VerifySpec {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Model-family effort rules.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EffortFamily {
    #[default]
    None,
    Openai,
    Xai,
}

impl EffortFamily {
    fn is_none(&self) -> bool {
        *self == Self::None
    }
}

fn is_zero(value: &u16) -> bool {
    *value == 0
}

fn default_auth_mode() -> AuthMode {
    AuthMode::ApiKey {
        tui_always_prompt: true,
    }
}

/// Who authored the spec.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecOrigin {
    #[default]
    Builtin,
    User,
}

/// One provider offering Forge can connect and run.
///
/// Renamed fields keep their old TOML names as aliases, and through stage 7
/// they are written under the old names too, so an older binary still reads
/// entries this one saves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderSpec {
    pub id: String,
    pub title: String,
    /// Optional display text; user provider files may omit it.
    #[serde(default)]
    pub description: String,
    /// Legacy auth form. When `auth` is set it is derived from it.
    #[serde(default = "default_auth_mode")]
    pub auth_mode: AuthMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthSpec>,
    /// Vendor env aliases for the key (first present wins).
    #[serde(default)]
    pub api_key_env: Vec<String>,
    /// Vendor base-URL env aliases.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub base_url_env: Vec<String>,
    /// Built-in profile whose credential slot this one shares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_from: Option<String>,
    #[serde(
        rename(serialize = "default_base_url", deserialize = "base_url"),
        alias = "default_base_url"
    )]
    pub base_url: String,
    /// Built-in fields a `[provider_overrides.<id>]` table may change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub editable: Vec<EditableField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire: Option<WireKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wire_overrides: Vec<WireOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach: Option<Attach>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub static_headers: Vec<StaticHeader>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_header: Option<SessionHeaderKind>,
    #[serde(default)]
    pub default_models: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manual_models: Vec<String>,
    #[serde(default, skip_serializing_if = "CatalogSpec::is_default")]
    pub catalog: CatalogSpec,
    #[serde(default, skip_serializing_if = "VerifySpec::is_default")]
    pub verify: VerifySpec,
    /// models.dev provider ids used for public model metadata and fallbacks.
    ///
    /// A Forge transport may have a distinct id: the ChatGPT Codex subscription
    /// transport, for example, uses the public OpenAI model registry.
    #[serde(default)]
    pub models_dev_providers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    /// Names other CLIs' login files use for this profile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub import_aliases: Vec<String>,
    /// Text shown when a key check fails.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub guidance: String,
    /// A shorter key is rejected before the probe.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub min_key_len: u16,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub billing_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub billing_note: String,
    #[serde(default, skip_serializing_if = "EffortFamily::is_none")]
    pub effort_family: EffortFamily,
    /// Legacy model-id prefix; equal to `id` for new custom profiles.
    #[serde(default)]
    pub model_provider_prefix: String,
    /// Display group for profiles that are one provider with several
    /// offerings (e.g. `openai` API key vs. ChatGPT sign-in). Equal to `id`
    /// for providers with only one offering.
    #[serde(
        default,
        rename(serialize = "vendor_id", deserialize = "provider"),
        alias = "vendor_id"
    )]
    pub provider: String,
    /// Group display name for the collapsed picker row, e.g. "OpenAI", the
    /// same value repeated across every profile sharing a `provider`.
    #[serde(
        default,
        rename(serialize = "vendor_label", deserialize = "provider_label"),
        alias = "vendor_label"
    )]
    pub provider_label: String,
    /// This profile's short label under its provider, e.g. "API key",
    /// "ChatGPT", "Go", "Zen". Empty for providers with one offering.
    #[serde(
        default,
        rename(serialize = "route_label", deserialize = "label"),
        alias = "route_label"
    )]
    pub label: String,
    /// Stable public route identity, e.g. `openai-chatgpt` (legacy).
    #[serde(default)]
    pub route_id: String,
    #[serde(default)]
    pub catalog_mode: CatalogMode,
    /// Legacy wire form. When `wire` is set it is derived from it.
    #[serde(default)]
    pub transport: ProviderTransport,
    #[serde(default, skip_serializing)]
    pub origin: SpecOrigin,
}

/// Historical name. Same type as [`ProviderSpec`].
pub type ConnectProfile = ProviderSpec;

impl ProviderSpec {
    /// A spec with every field at its default, for struct-update syntax.
    pub(crate) fn blank() -> Self {
        Self {
            id: String::new(),
            title: String::new(),
            description: String::new(),
            auth_mode: default_auth_mode(),
            auth: None,
            api_key_env: Vec::new(),
            base_url_env: Vec::new(),
            credential_from: None,
            base_url: String::new(),
            editable: Vec::new(),
            wire: None,
            wire_overrides: Vec::new(),
            attach: None,
            static_headers: Vec::new(),
            session_header: None,
            default_models: Vec::new(),
            manual_models: Vec::new(),
            catalog: CatalogSpec::default(),
            verify: VerifySpec::default(),
            models_dev_providers: Vec::new(),
            auth_url: None,
            import_aliases: Vec::new(),
            guidance: String::new(),
            min_key_len: 0,
            billing_url: String::new(),
            billing_note: String::new(),
            effort_family: EffortFamily::None,
            model_provider_prefix: String::new(),
            provider: String::new(),
            provider_label: String::new(),
            label: String::new(),
            route_id: String::new(),
            catalog_mode: CatalogMode::LiveRegistry,
            transport: ProviderTransport::OpenaiCompat,
            origin: SpecOrigin::Builtin,
        }
    }

    /// Stable public route identity.
    pub fn route_id(&self) -> &str {
        self.route_id.as_str()
    }

    /// The first model the profile lists: a shipped default, else one the
    /// user added.
    pub fn default_model(&self) -> Option<&str> {
        self.default_models
            .first()
            .or_else(|| self.manual_models.first())
            .map(|s| s.as_str())
    }

    /// True when this vendor has more than one offering registered alongside
    /// it — the only case where the picker nests routes under a chevron.
    pub fn has_multiple_routes(&self, registry: &crate::registry::ConnectRegistry) -> bool {
        registry
            .profiles()
            .iter()
            .filter(|p| p.provider == self.provider)
            .count()
            > 1
    }

    pub fn needs_tui_api_key_prompt(&self) -> bool {
        self.auth_mode.tui_always_prompt_key()
    }

    pub fn rejects_api_key_cli(&self) -> bool {
        self.auth_mode.is_oauth()
    }

    pub fn is_user(&self) -> bool {
        self.origin == SpecOrigin::User
    }

    /// The profile's wire protocol: `wire`, else mapped from `transport`.
    pub fn effective_wire(&self) -> WireKind {
        self.wire.unwrap_or_else(|| self.transport.into())
    }

    /// The profile's auth: `auth`, else mapped from `auth_mode`.
    pub fn effective_auth(&self) -> AuthSpec {
        self.auth
            .clone()
            .unwrap_or_else(|| AuthSpec::from(&self.auth_mode))
    }

    /// The catalog kind: `catalog.kind`, else from the wire
    /// (`anthropic-messages` → `anthropic-models`, otherwise
    /// `openai-models`).
    pub fn effective_catalog_kind(&self) -> CatalogKind {
        self.catalog.kind.unwrap_or(match self.effective_wire() {
            WireKind::AnthropicMessages => CatalogKind::AnthropicModels,
            _ => CatalogKind::OpenaiModels,
        })
    }

    /// The wire for one model: the first matching override, else the
    /// profile's wire.
    pub fn wire_for_model(&self, bare_model: &str) -> WireKind {
        self.wire_overrides
            .iter()
            .find(|entry| entry.matches(bare_model))
            .map(|entry| entry.wire)
            .unwrap_or_else(|| self.effective_wire())
    }
}

/// Result of a successful connect (never includes secret material).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectOutcome {
    pub profile_id: String,
    pub model: String,
    pub key_source: KeySource,
    /// False when the key was saved after a probe that neither accepted nor
    /// rejected it (not found, server error, unreachable).
    pub verified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Env,
    File,
    /// Provided just now (session) — treated as file after persist.
    Provided,
    /// OAuth access token stored.
    Oauth,
}

impl KeySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::File => "file",
            Self::Provided => "provided",
            Self::Oauth => "oauth",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectStatus {
    pub profile_id: Option<String>,
    pub model: Option<String>,
    pub key_source: Option<KeySource>,
    pub connected_profile_ids: Vec<String>,
}

#[cfg(test)]
pub(crate) fn test_spec(
    id: &str,
    auth_mode: AuthMode,
    default_models: Vec<String>,
) -> ProviderSpec {
    ProviderSpec {
        id: id.into(),
        title: id.into(),
        description: format!("{id} profile"),
        auth_mode,
        api_key_env: vec!["DEMO_KEY".into()],
        base_url: "https://example.test".into(),
        default_models,
        model_provider_prefix: id.into(),
        provider: id.into(),
        provider_label: id.into(),
        route_id: id.into(),
        ..ProviderSpec::blank()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(auth_mode: AuthMode, default_models: Vec<String>) -> ProviderSpec {
        test_spec("demo", auth_mode, default_models)
    }

    #[test]
    fn profile_helpers_reflect_auth_mode_and_default_model() {
        let api = profile(
            AuthMode::ApiKey {
                tui_always_prompt: true,
            },
            vec!["demo/model-a".into(), "demo/model-b".into()],
        );
        assert_eq!(api.default_model(), Some("demo/model-a"));
        assert!(api.needs_tui_api_key_prompt());
        assert!(!api.rejects_api_key_cli());

        let oauth = profile(
            AuthMode::Oauth {
                device_code: true,
                system_browser: false,
                auth_server: "https://issuer.example".into(),
            },
            vec![],
        );
        assert_eq!(oauth.default_model(), None);
        assert!(!oauth.needs_tui_api_key_prompt());
        assert!(oauth.rejects_api_key_cli());
    }

    #[test]
    fn key_source_labels_are_stable() {
        assert_eq!(KeySource::Env.as_str(), "env");
        assert_eq!(KeySource::File.as_str(), "file");
        assert_eq!(KeySource::Provided.as_str(), "provided");
        assert_eq!(KeySource::Oauth.as_str(), "oauth");
    }

    #[test]
    fn route_id_is_a_field_not_a_match() {
        let mut profile = profile(
            AuthMode::ApiKey {
                tui_always_prompt: true,
            },
            vec![],
        );
        profile.route_id = "openai-chatgpt".into();
        assert_eq!(profile.route_id(), "openai-chatgpt");
        profile.route_id = "openai-api".into();
        assert_eq!(profile.route_id(), "openai-api");
    }
}
