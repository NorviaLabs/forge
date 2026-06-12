//! Forge CLI — Phase 1 headless + interactive REPL surfaces (surfaces.md).

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use forge_config::{Config, ConfigOverrides};
use forge_core::{AgentSession, LoopConfig};
use forge_mcp::{register_static_mcp, McpManager, StaticMcpTool};
use forge_model::{client_from_config, MockModelClient, ModelClient};
use forge_tools::ToolRegistry;
use forge_tui::{help_text, parse_slash, ExitCode, SlashCommand};
use forge_types::ModelResponse;
use serde_json::json;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(name = "forge", version, about = "Forge AI agent harness (Phase 1)")]
struct Cli {
    /// Path to forge.toml
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Workspace root (default: cwd)
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,
    /// Model provider: openai_compatible | anthropic | xai
    #[arg(long, global = true)]
    provider: Option<String>,
    /// Model id
    #[arg(long, global = true)]
    model: Option<String>,
    /// Use mock model (no network)
    #[arg(long, global = true)]
    mock: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Headless single-shot (or multi-turn with --mock tools demo)
    Run {
        /// User prompt
        prompt: String,
        /// Resume session id
        #[arg(long)]
        resume: Option<Uuid>,
        /// Max agent turns
        #[arg(long, default_value_t = 8)]
        max_turns: u32,
    },
    /// Interactive REPL (simple TUI surface)
    Repl {
        #[arg(long)]
        resume: Option<Uuid>,
        #[arg(long, default_value_t = 16)]
        max_turns: u32,
    },
    /// Print version / health
    Status,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .with_writer(io::stderr)
        .init();

    let cli = Cli::parse();
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
        config_path: cli.config.clone(),
        workspace: cli.workspace.clone(),
        model_provider: cli.provider.clone(),
        model_id: cli.model.clone(),
        api_key: None,
        journal_path: None,
    };
    let cfg = Config::load(overrides).map_err(|e| anyhow::anyhow!(e))?;

    match cli.command {
        Commands::Status => {
            println!(
                "forge 0.1.0 phase1\nworkspace {}\nprovider {} model {}",
                cfg.workspace_root().display(),
                cfg.model.provider.as_str(),
                cfg.model.model
            );
            Ok(ExitCode::Success)
        }
        Commands::Run {
            prompt,
            resume,
            max_turns,
        } => {
            let mut session = open_session(&cfg, cli.mock, max_turns, resume).await?;
            let resp = session.run_user_message(&prompt).await?;
            print_response(&resp);
            println!("session_id={}", session.session_id);
            Ok(if session.status == forge_types::SessionStatus::Completed {
                ExitCode::Success
            } else {
                ExitCode::Failed
            })
        }
        Commands::Repl {
            resume,
            max_turns,
        } => {
            let mut session = open_session(&cfg, cli.mock, max_turns, resume).await?;
            println!("Forge REPL — session {}", session.session_id);
            println!("Type a message or /help. Ctrl-D to quit.\n");
            let stdin = io::stdin();
            let mut stdout = io::stdout();
            for line in stdin.lock().lines() {
                let line = line?;
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Some(cmd_res) = parse_slash(line) {
                    match cmd_res {
                        Ok(SlashCommand::Quit) => break,
                        Ok(SlashCommand::Help { .. }) => print!("{}", help_text()),
                        Ok(SlashCommand::Status) => {
                            println!(
                                "session={} status={:?} tools={}",
                                session.session_id,
                                session.status,
                                session.list_tools().len()
                            );
                        }
                        Ok(SlashCommand::Tools) => {
                            for n in session.list_tools() {
                                println!("  {n}");
                            }
                        }
                        Ok(SlashCommand::Resume { session_id }) => {
                            session =
                                open_session(&cfg, cli.mock, max_turns, Some(session_id)).await?;
                            println!("resumed {}", session.session_id);
                        }
                        Ok(SlashCommand::Cancel) => {
                            println!("cancel acknowledged (idle)");
                        }
                        Ok(SlashCommand::Model { provider, model }) => {
                            println!(
                                "model switch requested provider={provider:?} model={model:?} (restart to apply)"
                            );
                        }
                        Ok(SlashCommand::Journal { tail }) => {
                            let n = tail.unwrap_or(10);
                            println!("journal: last events in session (see .forge/sessions); showing recent agent events (up to {n})");
                            for e in session.events.iter().rev().take(n).collect::<Vec<_>>().into_iter().rev() {
                                println!("  [{}] {}", e.kind, e.detail);
                            }
                        }
                        Err(e) => println!("{e}"),
                    }
                    stdout.flush()?;
                    continue;
                }
                match session.run_user_message(line).await {
                    Ok(resp) => print_response(&resp),
                    Err(e) => println!("error: {e}"),
                }
                stdout.flush()?;
            }
            Ok(ExitCode::Success)
        }
    }
}

async fn open_session(
    cfg: &Config,
    mock: bool,
    max_turns: u32,
    resume: Option<Uuid>,
) -> anyhow::Result<AgentSession> {
    let model: Arc<dyn ModelClient> = if mock {
        Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "Mock model ready. (Use --mock for offline runs.)".into(),
            tool_calls: vec![],
            usage: None,
        }]))
    } else {
        match client_from_config(cfg) {
            Ok(c) => Arc::from(c),
            Err(e) => {
                eprintln!("warning: model client: {e}; falling back to --mock behavior");
                Arc::new(MockModelClient::script(vec![ModelResponse {
                    text: format!("(offline mock) model unavailable: {e}"),
                    tool_calls: vec![],
                    usage: None,
                }]))
            }
        }
    };

    let mut tools = ToolRegistry::new();
    // Demo static MCP tool always available (no subprocess) for CORE-02 path in tests/dev.
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

    // Optional real MCP servers from config
    if !cfg.mcp.servers.is_empty() {
        let mut mgr = McpManager::new();
        let errors = mgr.connect_all(&cfg.mcp.servers).await;
        for e in errors {
            eprintln!("mcp: {e}");
        }
        if let Err(e) = mgr.register_into(&mut tools).await {
            eprintln!("mcp register: {e}");
        }
    }

    let loop_cfg = LoopConfig {
        max_turns,
        workspace: cfg.workspace_root().to_path_buf(),
        journal_dir: cfg.journal_dir(),
    };

    let session = if let Some(id) = resume {
        AgentSession::resume(loop_cfg, model, tools, id).await?
    } else {
        AgentSession::create(loop_cfg, model, tools).await?
    };
    Ok(session)
}

fn print_response(resp: &ModelResponse) {
    if !resp.text.is_empty() {
        println!("{}", resp.text);
    }
    for tc in &resp.tool_calls {
        println!("[tool_call] {} {}", tc.name, tc.arguments);
    }
}
