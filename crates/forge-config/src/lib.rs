//! Phase 1 configuration: TOML + env overrides (configuration.md).

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
    #[error("invalid model provider `{0}` (expected openai_compatible | anthropic | xai)")]
    InvalidProvider(String),
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderKind {
    OpenaiCompatible,
    Anthropic,
    Xai,
}

impl ModelProviderKind {
    pub fn parse(s: &str) -> Result<Self, ConfigError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "openai_compatible" | "openai" | "openai-compatible" => Ok(Self::OpenaiCompatible),
            "anthropic" => Ok(Self::Anthropic),
            "xai" | "grok" => Ok(Self::Xai),
            other => Err(ConfigError::InvalidProvider(other.to_string())),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenaiCompatible => "openai_compatible",
            Self::Anthropic => "anthropic",
            Self::Xai => "xai",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub provider: ModelProviderKind,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Dev-only; prefer env `FORGE_API_KEY`. Never logged by callers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: ModelProviderKind::OpenaiCompatible,
            model: "gpt-4.1-mini".into(),
            base_url: None,
            api_key: None,
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
}

impl ConfigFile {
    fn apply(self, cfg: &mut Config) {
        if let Some(w) = self.workspace_root {
            cfg.workspace_root = Some(w);
        }
        if let Some(m) = self.model {
            if let Some(p) = m.provider {
                if let Ok(kind) = ModelProviderKind::parse(&p) {
                    cfg.model.provider = kind;
                }
            }
            if let Some(model) = m.model {
                cfg.model.model = model;
            }
            if m.base_url.is_some() {
                cfg.model.base_url = m.base_url;
            }
            if m.api_key.is_some() {
                cfg.model.api_key = m.api_key;
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

fn apply_env(cfg: &mut Config) -> Result<(), ConfigError> {
    if let Ok(p) = env::var("FORGE_MODEL_PROVIDER") {
        cfg.model.provider = ModelProviderKind::parse(&p)?;
    }
    if let Ok(m) = env::var("FORGE_MODEL_ID") {
        cfg.model.model = m;
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
    Ok(())
}

fn apply_overrides(cfg: &mut Config, o: &ConfigOverrides) -> Result<(), ConfigError> {
    if let Some(ref p) = o.model_provider {
        cfg.model.provider = ModelProviderKind::parse(p)?;
    }
    if let Some(ref m) = o.model_id {
        cfg.model.model = m.clone();
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
    use tempfile::tempdir;

    #[test]
    fn defaults_workspace_to_cwd() {
        let cfg = Config::load(ConfigOverrides::default()).unwrap();
        assert!(cfg.workspace_root().is_absolute() || cfg.workspace_root() == Path::new("."));
        assert_eq!(cfg.model.provider, ModelProviderKind::OpenaiCompatible);
        assert_eq!(cfg.journal.backend, "sqlite");
    }

    #[test]
    fn project_toml_overrides_defaults() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("forge.toml");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"
workspace_root = "{ws}"
[model]
provider = "anthropic"
model = "claude-sonnet"
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

        assert_eq!(cfg.model.provider, ModelProviderKind::Anthropic);
        assert_eq!(cfg.model.model, "claude-sonnet");
        assert_eq!(cfg.journal.path, "my-sessions");
        assert_eq!(cfg.mcp.servers.len(), 1);
        assert_eq!(cfg.mcp.servers[0].id, "demo");
        assert_eq!(cfg.resolved_workspace, dir.path());
    }

    #[test]
    fn env_overrides_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("forge.toml");
        fs::write(
            &path,
            r#"
[model]
provider = "anthropic"
model = "from-file"
"#,
        )
        .unwrap();

        // Safety: only set within test process; restore after.
        let prev = env::var("FORGE_MODEL_ID").ok();
        env::set_var("FORGE_MODEL_ID", "from-env");
        let cfg = Config::load(ConfigOverrides {
            config_path: Some(path),
            ..Default::default()
        })
        .unwrap();
        match prev {
            Some(v) => env::set_var("FORGE_MODEL_ID", v),
            None => env::remove_var("FORGE_MODEL_ID"),
        }

        assert_eq!(cfg.model.model, "from-env");
        assert_eq!(cfg.model.provider, ModelProviderKind::Anthropic);
    }

    #[test]
    fn cli_overrides_env() {
        let cfg = Config::load(ConfigOverrides {
            model_provider: Some("xai".into()),
            model_id: Some("grok".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(cfg.model.provider, ModelProviderKind::Xai);
        assert_eq!(cfg.model.model, "grok");
    }

    #[test]
    fn invalid_provider_errors() {
        let err = Config::load(ConfigOverrides {
            model_provider: Some("nope".into()),
            ..Default::default()
        })
        .unwrap_err();
        assert!(matches!(err, ConfigError::InvalidProvider(_)));
    }

    #[test]
    fn journal_dir_resolves_under_workspace() {
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
