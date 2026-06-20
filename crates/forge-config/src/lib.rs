//! Configuration: TOML + env overrides (Phase 1 merge rules + Phase 5 LiteLLM).

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
        "invalid model provider `{0}` (expected litellm | mock; deprecated openai_compatible|anthropic|xai migrate to litellm)"
    )]
    InvalidProvider(String),
    #[error("{0}")]
    Message(String),
}

/// Phase 5: sole production backend is LiteLLM; mock is offline CI only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderKind {
    Litellm,
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

    /// Accept Phase 5 kinds and migrate deprecated Phase 1 natives to LiteLLM.
    pub fn parse_with_migration(s: &str) -> Result<ParsedProvider, ConfigError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "litellm" => Ok(ParsedProvider {
                kind: Self::Litellm,
                model_prefix: None,
                migrated_from_native: false,
            }),
            "mock" => Ok(ParsedProvider {
                kind: Self::Mock,
                model_prefix: None,
                migrated_from_native: false,
            }),
            "openai_compatible" | "openai" | "openai-compatible" => Ok(ParsedProvider {
                kind: Self::Litellm,
                model_prefix: Some("openai"),
                migrated_from_native: true,
            }),
            "anthropic" => Ok(ParsedProvider {
                kind: Self::Litellm,
                model_prefix: Some("anthropic"),
                migrated_from_native: true,
            }),
            "xai" | "grok" => Ok(ParsedProvider {
                kind: Self::Litellm,
                model_prefix: Some("xai"),
                migrated_from_native: true,
            }),
            other => Err(ConfigError::InvalidProvider(other.to_string())),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Litellm => "litellm",
            Self::Mock => "mock",
        }
    }
}

/// Apply LiteLLM model-string migration for deprecated native providers.
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LitellmLifecycle {
    LongLived,
    PerCall,
}

impl Default for LitellmLifecycle {
    fn default() -> Self {
        Self::LongLived
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LitellmConfig {
    #[serde(default = "default_litellm_python")]
    pub python: String,
    #[serde(default = "default_litellm_module")]
    pub module: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_path: Option<String>,
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
    #[serde(default = "default_startup_timeout")]
    pub startup_timeout_secs: u64,
    #[serde(default)]
    pub lifecycle: LitellmLifecycle,
}

fn default_litellm_python() -> String {
    "python3".into()
}
fn default_litellm_module() -> String {
    "forge_litellm_worker".into()
}
fn default_request_timeout() -> u64 {
    120
}
fn default_startup_timeout() -> u64 {
    30
}

impl Default for LitellmConfig {
    fn default() -> Self {
        Self {
            python: default_litellm_python(),
            module: default_litellm_module(),
            worker_path: None,
            request_timeout_secs: default_request_timeout(),
            startup_timeout_secs: default_startup_timeout(),
            lifecycle: LitellmLifecycle::default(),
        }
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
    #[serde(default)]
    pub litellm: LitellmConfig,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: ModelProviderKind::Litellm,
            model: "openai/gpt-4.1-mini".into(),
            base_url: None,
            api_key: None,
            litellm: LitellmConfig::default(),
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TuiConfig {}

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

        let project = overrides
            .config_path
            .clone()
            .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join("forge.toml"));
        if project.is_file() {
            merge_file(&mut cfg, &project)?;
        }

        apply_env(&mut cfg)?;
        apply_overrides(&mut cfg, &overrides)?;

        let cwd = env::current_dir().map_err(ConfigError::Io)?;
        cfg.resolved_workspace = resolve_workspace(&cfg.workspace_root, overrides.workspace.as_deref(), &cwd);

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
    tui: Option<TuiConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct ModelConfigFile {
    provider: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    litellm: Option<LitellmConfigFile>,
}

#[derive(Debug, Default, Deserialize)]
struct LitellmConfigFile {
    python: Option<String>,
    module: Option<String>,
    worker_path: Option<String>,
    request_timeout_secs: Option<u64>,
    startup_timeout_secs: Option<u64>,
    lifecycle: Option<String>,
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
            if let Some(l) = m.litellm {
                apply_litellm_file(&mut cfg.model.litellm, l);
            }
        }
        if let Some(j) = self.journal {
            cfg.journal = j;
        }
        if let Some(mcp) = self.mcp {
            cfg.mcp = mcp;
        }
        if let Some(tui) = self.tui {
            cfg.tui = tui;
        }
    }
}

fn apply_litellm_file(dst: &mut LitellmConfig, src: LitellmConfigFile) {
    if let Some(p) = src.python {
        dst.python = p;
    }
    if let Some(m) = src.module {
        dst.module = m;
    }
    if src.worker_path.is_some() {
        dst.worker_path = src.worker_path;
    }
    if let Some(t) = src.request_timeout_secs {
        dst.request_timeout_secs = t;
    }
    if let Some(t) = src.startup_timeout_secs {
        dst.startup_timeout_secs = t;
    }
    if let Some(life) = src.lifecycle {
        dst.lifecycle = match life.to_ascii_lowercase().as_str() {
            "per_call" | "per-call" => LitellmLifecycle::PerCall,
            _ => LitellmLifecycle::LongLived,
        };
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
    if let Ok(p) = env::var("FORGE_LITELLM_PYTHON") {
        cfg.model.litellm.python = p;
    }
    if let Ok(m) = env::var("FORGE_LITELLM_MODULE") {
        cfg.model.litellm.module = m;
    }
    if let Ok(life) = env::var("FORGE_LITELLM_LIFECYCLE") {
        cfg.model.litellm.lifecycle = match life.to_ascii_lowercase().as_str() {
            "per_call" | "per-call" => LitellmLifecycle::PerCall,
            _ => LitellmLifecycle::LongLived,
        };
    }
    if let Ok(t) = env::var("FORGE_LITELLM_REQUEST_TIMEOUT_SECS") {
        if let Ok(n) = t.parse() {
            cfg.model.litellm.request_timeout_secs = n;
        }
    }
    if let Ok(t) = env::var("FORGE_LITELLM_STARTUP_TIMEOUT_SECS") {
        if let Ok(n) = t.parse() {
            cfg.model.litellm.startup_timeout_secs = n;
        }
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

fn resolve_workspace(
    from_cfg: &Option<String>,
    from_cli: Option<&Path>,
    cwd: &Path,
) -> PathBuf {
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
        "FORGE_LITELLM_PYTHON",
        "FORGE_LITELLM_MODULE",
        "FORGE_LITELLM_LIFECYCLE",
        "FORGE_LITELLM_REQUEST_TIMEOUT_SECS",
        "FORGE_LITELLM_STARTUP_TIMEOUT_SECS",
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
            Self {
                saved,
                _lock: lock,
            }
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
        assert_eq!(cfg.model.provider, ModelProviderKind::Litellm);
        assert_eq!(cfg.model.model, "openai/gpt-4.1-mini");
        assert_eq!(cfg.model.litellm.python, "python3");
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
provider = "litellm"
model = "anthropic/claude-sonnet"
[model.litellm]
python = "python3.12"
request_timeout_secs = 60
[journal]
path = "my-sessions"
[[mcp.servers]]
id = "demo"
command = "echo"
args = ["hi"]
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

        assert_eq!(cfg.model.provider, ModelProviderKind::Litellm);
        assert_eq!(cfg.model.model, "anthropic/claude-sonnet");
        assert_eq!(cfg.model.litellm.python, "python3.12");
        assert_eq!(cfg.model.litellm.request_timeout_secs, 60);
        assert_eq!(cfg.journal.path, "my-sessions");
        assert_eq!(cfg.mcp.servers.len(), 1);
        assert_eq!(cfg.mcp.servers[0].id, "demo");
        assert_eq!(cfg.resolved_workspace, dir.path());
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
        assert_eq!(cfg.model.provider, ModelProviderKind::Litellm);
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
        assert_eq!(cfg.model.provider, ModelProviderKind::Litellm);
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
    fn litellm_env_overrides() {
        let g = EnvGuard::clear_forge_env();
        g.set("FORGE_LITELLM_PYTHON", "/usr/bin/python3");
        g.set("FORGE_LITELLM_LIFECYCLE", "per_call");
        let cfg = Config::load(ConfigOverrides::default()).unwrap();
        assert_eq!(cfg.model.litellm.python, "/usr/bin/python3");
        assert_eq!(cfg.model.litellm.lifecycle, LitellmLifecycle::PerCall);
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
}
