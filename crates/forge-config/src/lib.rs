//! Configuration: TOML + env overrides.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error(
        "invalid model provider `{0}` (expected native | mock; legacy litellm/openai_compatible/anthropic/xai migrate to native)"
    )]
    InvalidProvider(String),
    #[error("{0}")]
    Message(String),
}

/// Native Rust is the production backend; mock is offline CI only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderKind {
    Native,
    Mock,
}

/// Result of parsing a provider string, including optional model-id migration prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedProvider {
    pub kind: ModelProviderKind,
    /// If set, prefix model id as `{prefix}/{model}` when model has no `/`.
    pub model_prefix: Option<&'static str>,
    /// True when the input used a Phase 1 native provider name.
    pub migrated_from_native: bool,
}

impl ModelProviderKind {
    pub fn parse(s: &str) -> Result<Self, ConfigError> {
        Ok(Self::parse_with_migration(s)?.kind)
    }

    /// Accept legacy names while routing live models through native Rust.
    pub fn parse_with_migration(s: &str) -> Result<ParsedProvider, ConfigError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "native" | "litellm" => Ok(ParsedProvider {
                kind: Self::Native,
                model_prefix: None,
                migrated_from_native: false,
            }),
            "mock" => Ok(ParsedProvider {
                kind: Self::Mock,
                model_prefix: None,
                migrated_from_native: false,
            }),
            "openai_compatible" | "openai" | "openai-compatible" => Ok(ParsedProvider {
                kind: Self::Native,
                model_prefix: Some("openai"),
                migrated_from_native: true,
            }),
            "anthropic" => Ok(ParsedProvider {
                kind: Self::Native,
                model_prefix: Some("anthropic"),
                migrated_from_native: true,
            }),
            "xai" | "grok" => Ok(ParsedProvider {
                kind: Self::Native,
                model_prefix: Some("xai"),
                migrated_from_native: true,
            }),
            other => Err(ConfigError::InvalidProvider(other.to_string())),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Mock => "mock",
        }
    }
}

/// Apply provider-prefix migration for deprecated provider-specific config.
pub fn migrate_model_id(model: &str, prefix: Option<&str>) -> String {
    let Some(prefix) = prefix else {
        return model.to_string();
    };
    if model.contains('/') {
        model.to_string()
    } else {
        format!("{prefix}/{model}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub provider: ModelProviderKind,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Dev-only; prefer env. Never logged by callers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
}

fn default_request_timeout() -> u64 {
    300
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: ModelProviderKind::Native,
            model: String::new(),
            base_url: None,
            api_key: None,
            request_timeout_secs: default_request_timeout(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalConfig {
    #[serde(default = "default_journal_backend")]
    pub backend: String,
    #[serde(default = "default_journal_path")]
    pub path: String,
}

fn default_journal_backend() -> String {
    "sqlite".into()
}
fn default_journal_path() -> String {
    ".forge/sessions".into()
}

impl Default for JournalConfig {
    fn default() -> Self {
        Self {
            backend: default_journal_backend(),
            path: default_journal_path(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpServerConfig {
    pub id: String,
    #[serde(default = "default_mcp_transport")]
    pub transport: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

fn default_mcp_transport() -> String {
    "stdio".into()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FileIconMode {
    #[default]
    Unicode,
    Off,
}

impl FileIconMode {
    pub fn parse(s: &str) -> Result<Self, ConfigError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "unicode" => Ok(Self::Unicode),
            "off" => Ok(Self::Off),
            other => Err(ConfigError::Message(format!(
                "invalid file_icons `{other}` (expected unicode | off)"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuiConfig {
    #[serde(default)]
    pub file_icons: FileIconMode,
    #[serde(default = "default_mouse_capture")]
    pub mouse_capture: bool,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            file_icons: FileIconMode::Unicode,
            mouse_capture: default_mouse_capture(),
        }
    }
}

fn default_mouse_capture() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CommandConfig {
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TuiConfigFile {
    file_icons: Option<String>,
    mouse_capture: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ValidationConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandConfig>,
}

/// Phase 9 — `[tools.web_search]` (WEB-01).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchProvider {
    #[default]
    Mock,
}

impl WebSearchProvider {
    pub fn parse(s: &str) -> Result<Self, ConfigError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mock" => Ok(Self::Mock),
            other => Err(ConfigError::Message(format!(
                "invalid web_search provider `{other}` (expected mock)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mock => "mock",
        }
    }

    /// Default env var name for the provider API key (mock has none).
    pub fn default_api_key_env(self) -> Option<&'static str> {
        match self {
            Self::Mock => None,
        }
    }

    pub fn needs_api_key(self) -> bool {
        !matches!(self, Self::Mock)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchConfig {
    #[serde(default = "default_web_search_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub provider: WebSearchProvider,
    /// Env var that holds the API key (ignored for mock).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default = "default_web_search_max_results")]
    pub max_results: u32,
    #[serde(default = "default_web_search_timeout_ms")]
    pub timeout_ms: u64,
    /// If true and provider needs a key that is missing, do not register the tool.
    #[serde(default = "default_web_search_require_key")]
    pub require_key: bool,
    #[serde(default = "default_web_search_max_query_chars")]
    pub max_query_chars: u32,
}

fn default_web_search_enabled() -> bool {
    true
}
fn default_web_search_max_results() -> u32 {
    8
}
fn default_web_search_timeout_ms() -> u64 {
    15_000
}
fn default_web_search_require_key() -> bool {
    true
}
fn default_web_search_max_query_chars() -> u32 {
    512
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            enabled: default_web_search_enabled(),
            provider: WebSearchProvider::Mock,
            api_key_env: None,
            max_results: default_web_search_max_results(),
            timeout_ms: default_web_search_timeout_ms(),
            require_key: default_web_search_require_key(),
            max_query_chars: default_web_search_max_query_chars(),
        }
    }
}

impl WebSearchConfig {
    /// Env name used to resolve the API key for the active provider.
    pub fn resolved_api_key_env(&self) -> Option<String> {
        if let Some(ref e) = self.api_key_env {
            if !e.is_empty() {
                return Some(e.clone());
            }
        }
        self.provider.default_api_key_env().map(str::to_string)
    }

    /// Whether `web_search` should be registered in the tool catalog.
    pub fn should_register(&self) -> bool {
        if !self.enabled {
            return false;
        }
        if !self.provider.needs_api_key() {
            return true;
        }
        if !self.require_key {
            return true;
        }
        self.api_key_present()
    }

    /// True if the configured API key env var is set and non-empty.
    pub fn api_key_present(&self) -> bool {
        let Some(env_name) = self.resolved_api_key_env() else {
            return false;
        };
        env::var(env_name)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolsConfig {
    #[serde(default)]
    pub web_search: WebSearchConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Optional; if unset/relative ".", resolved to process cwd at load time.
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub journal: JournalConfig,
    #[serde(default)]
    pub mcp: McpSection,
    #[serde(default)]
    pub tui: TuiConfig,
    #[serde(default)]
    pub validation: ValidationConfig,
    /// Phase 9 tool settings.
    #[serde(default)]
    pub tools: ToolsConfig,
    /// Resolved absolute workspace path (not from TOML alone).
    #[serde(skip)]
    pub resolved_workspace: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpSection {
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

// Support both [mcp] servers = [] and [[mcp.servers]] via flattened shape in TOML:
// We use:
// [mcp]
// [[mcp.servers]]
// which deserializes if we nest correctly — `mcp.servers` under [mcp] with array of tables.

impl Default for Config {
    fn default() -> Self {
        let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            workspace_root: None,
            model: ModelConfig::default(),
            journal: JournalConfig::default(),
            mcp: McpSection::default(),
            tui: TuiConfig::default(),
            validation: ValidationConfig::default(),
            tools: ToolsConfig::default(),
            resolved_workspace: cwd,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConfigOverrides {
    pub config_path: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
    pub model_provider: Option<String>,
    pub model_id: Option<String>,
    pub api_key: Option<String>,
    pub journal_path: Option<String>,
}

impl Config {
    pub fn workspace_root(&self) -> &Path {
        &self.resolved_workspace
    }

    pub fn journal_dir(&self) -> PathBuf {
        let p = PathBuf::from(&self.journal.path);
        if p.is_absolute() {
            p
        } else {
            self.resolved_workspace.join(p)
        }
    }

    /// Load with merge order: defaults < user XDG < project forge.toml < env < CLI overrides.
    pub fn load(overrides: ConfigOverrides) -> Result<Self, ConfigError> {
        let mut cfg = Config::default();

        if let Some(user) = user_config_path() {
            if user.is_file() {
                merge_file(&mut cfg, &user)?;
            }
        }

        let project = overrides.config_path.clone().unwrap_or_else(|| {
            env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("forge.toml")
        });
        if project.is_file() {
            merge_file(&mut cfg, &project)?;
        }

        apply_env(&mut cfg)?;
        apply_overrides(&mut cfg, &overrides)?;

        let cwd = env::current_dir().map_err(ConfigError::Io)?;
        cfg.resolved_workspace =
            resolve_workspace(&cfg.workspace_root, overrides.workspace.as_deref(), &cwd);

        Ok(cfg)
    }
}

fn user_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("forge").join("config.toml"))
}

fn merge_file(cfg: &mut Config, path: &Path) -> Result<(), ConfigError> {
    let text = fs::read_to_string(path)?;
    let partial: ConfigFile = toml::from_str(&text)?;
    partial.apply(cfg);
    Ok(())
}

/// TOML-facing shape (same fields as Config minus resolved_workspace).
#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    workspace_root: Option<String>,
    model: Option<ModelConfigFile>,
    journal: Option<JournalConfig>,
    mcp: Option<McpSection>,
    tui: Option<TuiConfigFile>,
    validation: Option<ValidationConfig>,
    tools: Option<ToolsConfigFile>,
}

#[derive(Debug, Default, Deserialize)]
struct ToolsConfigFile {
    web_search: Option<WebSearchConfigFile>,
}

#[derive(Debug, Default, Deserialize)]
struct WebSearchConfigFile {
    enabled: Option<bool>,
    provider: Option<String>,
    api_key_env: Option<String>,
    max_results: Option<u32>,
    timeout_ms: Option<u64>,
    require_key: Option<bool>,
    max_query_chars: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
struct ModelConfigFile {
    provider: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    request_timeout_secs: Option<u64>,
}

impl ConfigFile {
    fn apply(self, cfg: &mut Config) {
        if let Some(w) = self.workspace_root {
            cfg.workspace_root = Some(w);
        }
        if let Some(m) = self.model {
            let mut prefix = None;
            if let Some(p) = m.provider {
                if let Ok(parsed) = ModelProviderKind::parse_with_migration(&p) {
                    cfg.model.provider = parsed.kind;
                    prefix = parsed.model_prefix;
                }
            }
            if let Some(model) = m.model {
                cfg.model.model = migrate_model_id(&model, prefix);
            } else if let Some(p) = prefix {
                cfg.model.model = migrate_model_id(&cfg.model.model, Some(p));
            }
            if m.base_url.is_some() {
                cfg.model.base_url = m.base_url;
            }
            if m.api_key.is_some() {
                cfg.model.api_key = m.api_key;
            }
            if let Some(timeout) = m.request_timeout_secs {
                cfg.model.request_timeout_secs = timeout.max(1);
            }
        }
        if let Some(j) = self.journal {
            cfg.journal = j;
        }
        if let Some(mcp) = self.mcp {
            cfg.mcp = mcp;
        }
        if let Some(tui) = self.tui {
            if let Some(file_icons) = tui.file_icons {
                if let Ok(mode) = FileIconMode::parse(&file_icons) {
                    cfg.tui.file_icons = mode;
                }
            }
            if let Some(mouse_capture) = tui.mouse_capture {
                cfg.tui.mouse_capture = mouse_capture;
            }
        }
        if let Some(validation) = self.validation {
            cfg.validation = validation;
        }
        if let Some(tools) = self.tools {
            if let Some(ws) = tools.web_search {
                apply_web_search_file(&mut cfg.tools.web_search, ws);
            }
        }
    }
}

fn apply_web_search_file(dst: &mut WebSearchConfig, src: WebSearchConfigFile) {
    if let Some(e) = src.enabled {
        dst.enabled = e;
    }
    if let Some(p) = src.provider {
        if let Ok(prov) = WebSearchProvider::parse(&p) {
            dst.provider = prov;
        }
    }
    if src.api_key_env.is_some() {
        dst.api_key_env = src.api_key_env;
    }
    if let Some(n) = src.max_results {
        dst.max_results = n.max(1);
    }
    if let Some(t) = src.timeout_ms {
        dst.timeout_ms = t;
    }
    if let Some(r) = src.require_key {
        dst.require_key = r;
    }
    if let Some(m) = src.max_query_chars {
        dst.max_query_chars = m.max(1);
    }
}

fn apply_env(cfg: &mut Config) -> Result<(), ConfigError> {
    let mut prefix = None;
    if let Ok(p) = env::var("FORGE_MODEL_PROVIDER") {
        let parsed = ModelProviderKind::parse_with_migration(&p)?;
        cfg.model.provider = parsed.kind;
        prefix = parsed.model_prefix;
    }
    if let Ok(m) = env::var("FORGE_MODEL_ID") {
        cfg.model.model = migrate_model_id(&m, prefix);
    } else if let Some(p) = prefix {
        cfg.model.model = migrate_model_id(&cfg.model.model, Some(p));
    }
    if let Ok(k) = env::var("FORGE_API_KEY") {
        cfg.model.api_key = Some(k);
    }
    if let Ok(w) = env::var("FORGE_WORKSPACE") {
        cfg.workspace_root = Some(w);
    }
    if let Ok(j) = env::var("FORGE_JOURNAL_PATH") {
        cfg.journal.path = j;
    }
    if let Ok(t) = env::var("FORGE_MODEL_REQUEST_TIMEOUT_SECS") {
        if let Ok(n) = t.parse() {
            cfg.model.request_timeout_secs = n;
        }
    }
    // Phase 9 — web_search
    if let Ok(v) = env::var("FORGE_WEB_SEARCH_ENABLED") {
        cfg.tools.web_search.enabled =
            matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on");
    }
    if let Ok(p) = env::var("FORGE_WEB_SEARCH_PROVIDER") {
        cfg.tools.web_search.provider = WebSearchProvider::parse(&p)?;
    }
    if let Ok(e) = env::var("FORGE_WEB_SEARCH_API_KEY_ENV") {
        cfg.tools.web_search.api_key_env = Some(e);
    }
    if let Ok(n) = env::var("FORGE_WEB_SEARCH_MAX_RESULTS") {
        if let Ok(n) = n.parse::<u32>() {
            cfg.tools.web_search.max_results = n.max(1);
        }
    }
    if let Ok(t) = env::var("FORGE_WEB_SEARCH_TIMEOUT_MS") {
        if let Ok(t) = t.parse::<u64>() {
            cfg.tools.web_search.timeout_ms = t;
        }
    }
    if let Ok(v) = env::var("FORGE_WEB_SEARCH_REQUIRE_KEY") {
        cfg.tools.web_search.require_key =
            matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on");
    }
    Ok(())
}

fn apply_overrides(cfg: &mut Config, o: &ConfigOverrides) -> Result<(), ConfigError> {
    let mut prefix = None;
    if let Some(ref p) = o.model_provider {
        let parsed = ModelProviderKind::parse_with_migration(p)?;
        cfg.model.provider = parsed.kind;
        prefix = parsed.model_prefix;
    }
    if let Some(ref m) = o.model_id {
        cfg.model.model = migrate_model_id(m, prefix);
    } else if let Some(p) = prefix {
        cfg.model.model = migrate_model_id(&cfg.model.model, Some(p));
    }
    if let Some(ref k) = o.api_key {
        cfg.model.api_key = Some(k.clone());
    }
    if let Some(ref j) = o.journal_path {
        cfg.journal.path = j.clone();
    }
    if let Some(ref w) = o.workspace {
        cfg.workspace_root = Some(w.display().to_string());
    }
    Ok(())
}

fn resolve_workspace(from_cfg: &Option<String>, from_cli: Option<&Path>, cwd: &Path) -> PathBuf {
    if let Some(cli) = from_cli {
        return if cli.is_absolute() {
            cli.to_path_buf()
        } else {
            cwd.join(cli)
        };
    }
    match from_cfg.as_deref() {
        None | Some(".") | Some("") => cwd.to_path_buf(),
        Some(p) => {
            let path = PathBuf::from(p);
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;
    use tempfile::tempdir;

    /// Serializes tests that touch process env (rustc may run tests in parallel).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    const FORGE_ENV_KEYS: &[&str] = &[
        "FORGE_MODEL_PROVIDER",
        "FORGE_MODEL_ID",
        "FORGE_API_KEY",
        "FORGE_WORKSPACE",
        "FORGE_JOURNAL_PATH",
        "FORGE_OTEL_ENDPOINT",
        "FORGE_MODEL_REQUEST_TIMEOUT_SECS",
        "FORGE_WEB_SEARCH_ENABLED",
        "FORGE_WEB_SEARCH_PROVIDER",
        "FORGE_WEB_SEARCH_API_KEY_ENV",
        "FORGE_WEB_SEARCH_MAX_RESULTS",
        "FORGE_WEB_SEARCH_TIMEOUT_MS",
        "FORGE_WEB_SEARCH_REQUIRE_KEY",
        "TAVILY_API_KEY",
        "BRAVE_API_KEY",
        "SERPER_API_KEY",
    ];

    /// Clears FORGE_* env vars for the duration of a test; restores on drop.
    struct EnvGuard {
        saved: Vec<(String, Option<String>)>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn clear_forge_env() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let mut saved = Vec::new();
            for key in FORGE_ENV_KEYS {
                saved.push(((*key).to_string(), env::var(key).ok()));
                env::remove_var(key);
            }
            Self { saved, _lock: lock }
        }

        fn set(&self, key: &str, value: &str) {
            env::set_var(key, value);
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, val) in self.saved.drain(..) {
                match val {
                    Some(v) => env::set_var(&key, v),
                    None => env::remove_var(&key),
                }
            }
        }
    }

    #[test]
    fn defaults_workspace_to_cwd() {
        let _g = EnvGuard::clear_forge_env();
        let cfg = Config::load(ConfigOverrides::default()).unwrap();
        assert!(cfg.workspace_root().is_absolute() || cfg.workspace_root() == Path::new("."));
        assert_eq!(cfg.model.provider, ModelProviderKind::Native);
        assert!(cfg.model.model.is_empty());
        assert_eq!(cfg.model.request_timeout_secs, 300);
        assert_eq!(cfg.journal.backend, "sqlite");
    }

    #[test]
    fn project_toml_overrides_defaults() {
        let _g = EnvGuard::clear_forge_env();
        let dir = tempdir().unwrap();
        let path = dir.path().join("forge.toml");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"
workspace_root = "{ws}"
[model]
provider = "native"
model = "anthropic/claude-sonnet"
request_timeout_secs = 60
[journal]
path = "my-sessions"
[[mcp.servers]]
id = "demo"
command = "echo"
args = ["hi"]
[tui]
mouse_capture = false
"#,
            ws = dir.path().display()
        )
        .unwrap();

        let cfg = Config::load(ConfigOverrides {
            config_path: Some(path),
            workspace: None,
            ..Default::default()
        })
        .unwrap();

        assert_eq!(cfg.model.provider, ModelProviderKind::Native);
        assert_eq!(cfg.model.model, "anthropic/claude-sonnet");
        assert_eq!(cfg.model.request_timeout_secs, 60);
        assert_eq!(cfg.journal.path, "my-sessions");
        assert_eq!(cfg.mcp.servers.len(), 1);
        assert_eq!(cfg.mcp.servers[0].id, "demo");
        assert_eq!(cfg.resolved_workspace, dir.path());
        assert!(!cfg.tui.mouse_capture);
    }

    #[test]
    fn migrates_deprecated_anthropic_provider() {
        let _g = EnvGuard::clear_forge_env();
        let dir = tempdir().unwrap();
        let path = dir.path().join("forge.toml");
        fs::write(
            &path,
            r#"
[model]
provider = "anthropic"
model = "claude-sonnet"
"#,
        )
        .unwrap();
        let cfg = Config::load(ConfigOverrides {
            config_path: Some(path),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(cfg.model.provider, ModelProviderKind::Native);
        assert_eq!(cfg.model.model, "anthropic/claude-sonnet");
    }

    #[test]
    fn migrate_model_id_skips_when_already_slashed() {
        assert_eq!(
            migrate_model_id("anthropic/claude", Some("anthropic")),
            "anthropic/claude"
        );
        assert_eq!(
            migrate_model_id("claude", Some("anthropic")),
            "anthropic/claude"
        );
        assert_eq!(migrate_model_id("gpt-5", None), "gpt-5");
    }

    #[test]
    fn provider_parse_accepts_legacy_aliases_and_reports_canonical_names() {
        for (raw, prefix, migrated) in [
            ("native", None, false),
            ("litellm", None, false),
            ("openai_compatible", Some("openai"), true),
            ("openai-compatible", Some("openai"), true),
            ("openai", Some("openai"), true),
            ("anthropic", Some("anthropic"), true),
            ("xai", Some("xai"), true),
            ("grok", Some("xai"), true),
        ] {
            let parsed = ModelProviderKind::parse_with_migration(raw).unwrap();
            assert_eq!(parsed.kind, ModelProviderKind::Native);
            assert_eq!(parsed.model_prefix, prefix);
            assert_eq!(parsed.migrated_from_native, migrated);
        }
        assert_eq!(ModelProviderKind::Mock.as_str(), "mock");
        assert_eq!(ModelProviderKind::Native.as_str(), "native");
    }

    #[test]
    fn env_overrides_file() {
        let g = EnvGuard::clear_forge_env();
        let dir = tempdir().unwrap();
        let path = dir.path().join("forge.toml");
        fs::write(
            &path,
            r#"
[model]
provider = "litellm"
model = "from-file"
"#,
        )
        .unwrap();

        g.set("FORGE_MODEL_ID", "openai/from-env");
        let cfg = Config::load(ConfigOverrides {
            config_path: Some(path),
            ..Default::default()
        })
        .unwrap();

        assert_eq!(cfg.model.model, "openai/from-env");
        assert_eq!(cfg.model.provider, ModelProviderKind::Native);
    }

    #[test]
    fn cli_overrides_env() {
        let g = EnvGuard::clear_forge_env();
        g.set("FORGE_MODEL_PROVIDER", "litellm");
        g.set("FORGE_MODEL_ID", "from-env");
        let cfg = Config::load(ConfigOverrides {
            model_provider: Some("mock".into()),
            model_id: Some("mock".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(cfg.model.provider, ModelProviderKind::Mock);
        assert_eq!(cfg.model.model, "mock");
    }

    #[test]
    fn model_timeout_env_overrides() {
        let g = EnvGuard::clear_forge_env();
        g.set("FORGE_MODEL_REQUEST_TIMEOUT_SECS", "42");
        let cfg = Config::load(ConfigOverrides::default()).unwrap();
        assert_eq!(cfg.model.request_timeout_secs, 42);
    }

    #[test]
    fn invalid_provider_errors() {
        let _g = EnvGuard::clear_forge_env();
        let err = Config::load(ConfigOverrides {
            model_provider: Some("nope".into()),
            ..Default::default()
        })
        .unwrap_err();
        assert!(matches!(err, ConfigError::InvalidProvider(_)));
    }

    #[test]
    fn journal_dir_resolves_under_workspace() {
        let _g = EnvGuard::clear_forge_env();
        let dir = tempdir().unwrap();
        let cfg = Config::load(ConfigOverrides {
            workspace: Some(dir.path().to_path_buf()),
            journal_path: Some("j".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(cfg.journal_dir(), dir.path().join("j"));
    }

    #[test]
    fn journal_dir_keeps_absolute_path() {
        let _g = EnvGuard::clear_forge_env();
        let dir = tempdir().unwrap();
        let journal = dir.path().join("absolute-journal");
        let cfg = Config::load(ConfigOverrides {
            workspace: Some(dir.path().to_path_buf()),
            journal_path: Some(journal.display().to_string()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(cfg.journal_dir(), journal);
    }

    #[test]
    fn web_search_defaults_to_enabled_mock() {
        let _g = EnvGuard::clear_forge_env();
        let cfg = Config::load(ConfigOverrides::default()).unwrap();
        assert!(cfg.tools.web_search.enabled);
        assert_eq!(cfg.tools.web_search.provider, WebSearchProvider::Mock);
        assert_eq!(cfg.tools.web_search.max_results, 8);
        assert_eq!(cfg.tools.web_search.timeout_ms, 15_000);
        assert!(cfg.tools.web_search.require_key);
        assert!(cfg.tools.web_search.should_register());
    }

    #[test]
    fn web_search_toml_section() {
        let _g = EnvGuard::clear_forge_env();
        let dir = tempdir().unwrap();
        let path = dir.path().join("forge.toml");
        fs::write(
            &path,
            r#"
[tools.web_search]
enabled = true
provider = "mock"
api_key_env = "MY_TAVILY"
max_results = 3
timeout_ms = 9000
require_key = false
max_query_chars = 200
"#,
        )
        .unwrap();
        let cfg = Config::load(ConfigOverrides {
            config_path: Some(path),
            ..Default::default()
        })
        .unwrap();
        let ws = &cfg.tools.web_search;
        assert!(ws.enabled);
        assert_eq!(ws.provider, WebSearchProvider::Mock);
        assert_eq!(ws.api_key_env.as_deref(), Some("MY_TAVILY"));
        assert_eq!(ws.max_results, 3);
        assert_eq!(ws.timeout_ms, 9000);
        assert!(!ws.require_key);
        assert_eq!(ws.max_query_chars, 200);
        assert_eq!(ws.resolved_api_key_env().as_deref(), Some("MY_TAVILY"));
    }

    #[test]
    fn web_search_toml_clamps_minimums_and_allows_empty_api_key_env() {
        let _g = EnvGuard::clear_forge_env();
        let dir = tempdir().unwrap();
        let path = dir.path().join("forge.toml");
        fs::write(
            &path,
            r#"
[tools.web_search]
api_key_env = ""
max_results = 0
max_query_chars = 0
"#,
        )
        .unwrap();
        let cfg = Config::load(ConfigOverrides {
            config_path: Some(path),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(cfg.tools.web_search.max_results, 1);
        assert_eq!(cfg.tools.web_search.max_query_chars, 1);
        assert!(cfg.tools.web_search.resolved_api_key_env().is_none());
    }

    #[test]
    fn web_search_env_overrides() {
        let g = EnvGuard::clear_forge_env();
        g.set("FORGE_WEB_SEARCH_ENABLED", "false");
        g.set("FORGE_WEB_SEARCH_PROVIDER", "mock");
        g.set("FORGE_WEB_SEARCH_MAX_RESULTS", "2");
        let cfg = Config::load(ConfigOverrides::default()).unwrap();
        assert!(!cfg.tools.web_search.enabled);
        assert_eq!(cfg.tools.web_search.provider, WebSearchProvider::Mock);
        assert_eq!(cfg.tools.web_search.max_results, 2);
        assert!(!cfg.tools.web_search.should_register());
    }

    #[test]
    fn web_search_env_overrides_limits_and_key_options() {
        let g = EnvGuard::clear_forge_env();
        g.set("FORGE_WEB_SEARCH_API_KEY_ENV", "SEARCH_KEY");
        g.set("FORGE_WEB_SEARCH_MAX_RESULTS", "0");
        g.set("FORGE_WEB_SEARCH_TIMEOUT_MS", "1234");
        g.set("FORGE_WEB_SEARCH_REQUIRE_KEY", "off");
        let cfg = Config::load(ConfigOverrides::default()).unwrap();
        let ws = cfg.tools.web_search;
        assert_eq!(ws.api_key_env.as_deref(), Some("SEARCH_KEY"));
        assert_eq!(ws.max_results, 1);
        assert_eq!(ws.timeout_ms, 1234);
        assert!(!ws.require_key);
    }

    #[test]
    fn web_search_mock_does_not_require_key() {
        let g = EnvGuard::clear_forge_env();
        let mut ws = WebSearchConfig {
            enabled: true,
            provider: WebSearchProvider::Mock,
            require_key: true,
            ..Default::default()
        };
        assert!(ws.should_register());
        g.set("TAVILY_API_KEY", "secret-test-key");
        assert!(ws.should_register());
        ws.require_key = false;
        env::remove_var("TAVILY_API_KEY");
        assert!(ws.should_register());
    }

    #[test]
    fn web_search_provider_parse() {
        assert_eq!(
            WebSearchProvider::parse("MOCK").unwrap(),
            WebSearchProvider::Mock
        );
        assert!(WebSearchProvider::parse("bing").is_err());
    }
}
