//! Forge CLI — launches the full-screen TUI by default.

use std::sync::Arc;

use clap::Parser;
use forge_config::{Config, ConfigOverrides};
use forge_core::{AgentSession, LoopConfig};
use forge_mcp::{register_static_mcp, McpManager, StaticMcpTool};
use forge_model::{client_from_config, ModelClient};
use forge_tools::ToolRegistry;
use forge_tui::{run_tui, ExitCode, TuiRuntimeConfig};
use forge_types::SessionId;
use serde_json::json;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "forge",
    version,
    about = "Forge AI coding agent",
    long_about = "Open the full-screen TUI by default.\n\nUse --help or --version for CLI info."
)]
struct Cli {
    #[arg(long = "resume", value_name = "SESSION_ID")]
    resume: Option<SessionId>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("error")),
        )
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_ansi(false)
        .init();

    let code = match run(cli).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::Failed
        }
    };
    std::process::exit(code.code());
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let overrides = ConfigOverrides {
        config_path: None,
        workspace: None,
        model_provider: None,
        model_id: None,
        api_key: None,
        journal_path: None,
    };
    let cfg = Config::load(overrides).map_err(|e| anyhow::anyhow!(e))?;

    let (session, startup_notices) = open_session(&cfg, cli.resume).await?;
    let runtime = TuiRuntimeConfig {
        model_label: cfg.model.model.clone(),
        provider: cfg.model.provider.as_str().into(),
        cwd: cfg.workspace_root().to_path_buf(),
        version: env!("CARGO_PKG_VERSION").into(),
        startup_notices,
        validation_command: cfg.validation.command.clone(),
        file_icons: cfg.tui.file_icons,
        mouse_capture: cfg.tui.mouse_capture,
    };
    let summary = run_tui(session, runtime)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    if let Some(token_usage) = summary.token_usage {
        println!("{token_usage}");
        println!(
            "To continue this session, run forge --resume {}",
            summary.session_id
        );
    }
    Ok(summary.exit_code)
}

/// Load OAuth / API keys from the connect store into the native model client environment.
fn inject_connect_credentials_into_env() {
    let reg = forge_connect::builtin_registry();
    let store = forge_connect::CredentialStore::user_default();
    let svc = forge_connect::ConnectService {
        registry: &reg,
        store: &store,
        active_profile_id: None,
        active_model: None,
    };
    for p in reg.profiles() {
        if let Ok(pairs) = svc.provider_env_for_profile(&p.id) {
            for (k, v) in pairs {
                if std::env::var(&k).ok().filter(|s| !s.is_empty()).is_none() {
                    std::env::set_var(k, v);
                }
            }
        }
    }
}

async fn open_session(
    cfg: &Config,
    resume: Option<SessionId>,
) -> anyhow::Result<(AgentSession, Vec<String>)> {
    inject_connect_credentials_into_env();

    let model: Arc<dyn ModelClient> =
        Arc::from(client_from_config(cfg).map_err(|e| anyhow::anyhow!(e))?);

    let mut tools = ToolRegistry::new();
    register_static_mcp(
        &mut tools,
        "demo",
        vec![StaticMcpTool {
            server_id: "demo".into(),
            tool_name: "echo".into(),
            description: "Echo text (static MCP demo)".into(),
            schema: json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }),
            handler: Box::new(|args| forge_types::ToolOutput {
                content: args
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
                is_error: false,
            }),
        }],
    );

    let mut startup_notices = Vec::new();
    if !cfg.mcp.servers.is_empty() {
        let mut mgr = McpManager::new();
        let errors = mgr.connect_all(&cfg.mcp.servers).await;
        for e in errors {
            startup_notices.push(format!("mcp: {e}"));
        }
        let _ = mgr.register_into(&mut tools).await;
    }

    let loop_cfg = LoopConfig {
        max_turns: 128,
        workspace: cfg.workspace_root().to_path_buf(),
        journal_dir: cfg.journal_dir(),
        enable_context_lifecycle: true,
        enable_governance: true,
        web_search: cfg.tools.web_search.clone(),
        ..Default::default()
    };

    let mut session = AgentSession::create(loop_cfg, model, tools)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    if let Some(session_id) = resume {
        session
            .resume_session(session_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
    }
    if !cfg.model.model.is_empty() {
        session.set_active_model(cfg.model.model.clone());
    }
    Ok((session, startup_notices))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn cli_resume_parses_session_id() {
        let session_id = SessionId::new_v4();
        let cli = Cli::try_parse_from(["forge", "--resume", &session_id.to_string()]).unwrap();
        assert_eq!(cli.resume, Some(session_id));
    }

    #[tokio::test]
    async fn open_session_with_mock_model_builds_a_session_without_notices() {
        let temp = TempDir::new().unwrap();
        let mut cfg = Config::default();
        cfg.model.provider = forge_config::ModelProviderKind::Mock;
        cfg.model.model = "mock".into();
        cfg.resolved_workspace = temp.path().to_path_buf();
        cfg.workspace_root = Some(temp.path().display().to_string());

        let (session, notices) = open_session(&cfg, None).await.unwrap();
        assert!(notices.is_empty());
        assert_eq!(session.session_id.to_string().len(), 36);
    }
}
