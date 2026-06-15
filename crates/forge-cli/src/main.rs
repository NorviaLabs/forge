//! Forge CLI — Phase 1 + Phase 2 surfaces.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use forge_config::{Config, ConfigOverrides};
use forge_core::{AgentSession, LoopConfig};
use forge_mcp::{register_static_mcp, McpManager, StaticMcpTool};
use forge_model::{client_from_config, MockModelClient, ModelClient};
use forge_tools::ToolRegistry;
use forge_tui::{help_text, parse_slash, ExitCode, SlashCommand, WorktreeAction};
use forge_types::{HitlDecision, ModelResponse, SessionStatus};
use forge_workspace::IsolationMode;
use serde_json::json;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(name = "forge", version, about = "Forge AI agent harness (Phase 1–2)")]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,
    #[arg(long, global = true)]
    provider: Option<String>,
    #[arg(long, global = true)]
    model: Option<String>,
    #[arg(long, global = true)]
    mock: bool,
    /// Enable git worktree isolation (Phase 2 CTX-03)
    #[arg(long, global = true)]
    worktree: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Run {
        prompt: String,
        #[arg(long)]
        resume: Option<Uuid>,
        #[arg(long, default_value_t = 8)]
        max_turns: u32,
    },
    Repl {
        #[arg(long)]
        resume: Option<Uuid>,
        #[arg(long, default_value_t = 16)]
        max_turns: u32,
    },
    Status,
    /// Phase 2: resolve HITL for a resumed session
    Approve {
        #[arg(long)]
        session: Uuid,
    },
    Deny {
        #[arg(long)]
        session: Uuid,
    },
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
                "forge 0.2.0 phase2\nworkspace {}\nprovider {} model {}",
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
            let mut session =
                open_session(&cfg, cli.mock, max_turns, resume, cli.worktree).await?;
            let _resp = session.run_user_message(&prompt).await?;
            print_session_tail(&session);
            println!("session_id={}", session.session_id);
            Ok(exit_for_status(session.status))
        }
        Commands::Repl {
            resume,
            max_turns,
        } => {
            let mut session =
                open_session(&cfg, cli.mock, max_turns, resume, cli.worktree).await?;
            println!("Forge REPL — session {}", session.session_id);
            println!("{}", help_text());
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
                                "session={} status={:?} tools={} ctx_usage={:.1}% hitl={:?}",
                                session.session_id,
                                session.status,
                                session.list_tools().len(),
                                session.context_usage_ratio() * 100.0,
                                session.pending_hitl.as_ref().map(|p| &p.tool)
                            );
                        }
                        Ok(SlashCommand::Tools) => {
                            for n in session.list_tools() {
                                println!("  {n}");
                            }
                        }
                        Ok(SlashCommand::Resume { session_id }) => {
                            session = open_session(
                                &cfg,
                                cli.mock,
                                max_turns,
                                Some(session_id),
                                cli.worktree,
                            )
                            .await?;
                            println!("resumed {}", session.session_id);
                        }
                        Ok(SlashCommand::Cancel) => println!("cancel acknowledged"),
                        Ok(SlashCommand::Model { provider, model }) => {
                            println!("model switch requested provider={provider:?} model={model:?}");
                        }
                        Ok(SlashCommand::Journal { tail }) => {
                            let n = tail.unwrap_or(10);
                            for e in session
                                .events
                                .iter()
                                .rev()
                                .take(n)
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                            {
                                println!("  [{}] {}", e.kind, e.detail);
                            }
                        }
                        Ok(SlashCommand::Approve) => {
                            session
                                .resolve_hitl(HitlDecision::Approve, "tui")
                                .await?;
                            println!("approved");
                        }
                        Ok(SlashCommand::Deny) => {
                            session.resolve_hitl(HitlDecision::Deny, "tui").await?;
                            println!("denied");
                        }
                        Ok(SlashCommand::Reset) | Ok(SlashCommand::Compact) => {
                            session.force_context_reset_async().await?;
                            println!("context reset + progress.json written");
                        }
                        Ok(SlashCommand::Cost) => {
                            println!(
                                "context usage ratio: {:.2}%",
                                session.context_usage_ratio() * 100.0
                            );
                        }
                        Ok(SlashCommand::Worktree { action }) => match action {
                            WorktreeAction::Status => {
                                println!(
                                    "{}",
                                    session
                                        .worktree_status()
                                        .unwrap_or_else(|| "worktree off".into())
                                );
                            }
                            WorktreeAction::Merge => {
                                session.worktree_merge()?;
                                println!("worktree merged");
                            }
                            WorktreeAction::Discard { confirm } => {
                                if !confirm {
                                    println!("confirm with /worktree discard --yes");
                                } else {
                                    session.worktree_discard()?;
                                    println!("worktree discarded");
                                }
                            }
                        },
                        Err(e) => println!("{e}"),
                    }
                    stdout.flush()?;
                    continue;
                }
                match session.run_user_message(line).await {
                    Ok(resp) => {
                        print_response(&resp);
                        if session.status == SessionStatus::AwaitingHitl {
                            println!("(awaiting HITL — /approve or /deny)");
                        }
                    }
                    Err(e) => println!("error: {e}"),
                }
                stdout.flush()?;
            }
            Ok(ExitCode::Success)
        }
        Commands::Approve { session } => {
            let mut s = open_session(&cfg, cli.mock, 8, Some(session), cli.worktree).await?;
            s.resolve_hitl(HitlDecision::Approve, "cli").await?;
            println!("approved");
            Ok(ExitCode::Success)
        }
        Commands::Deny { session } => {
            let mut s = open_session(&cfg, cli.mock, 8, Some(session), cli.worktree).await?;
            s.resolve_hitl(HitlDecision::Deny, "cli").await?;
            println!("denied");
            Ok(ExitCode::Success)
        }
    }
}

fn exit_for_status(status: SessionStatus) -> ExitCode {
    match status {
        SessionStatus::Completed => ExitCode::Success,
        SessionStatus::AwaitingHitl => ExitCode::AwaitingHitl,
        SessionStatus::Failed => ExitCode::Failed,
        SessionStatus::Running => ExitCode::Success,
    }
}

fn print_session_tail(session: &AgentSession) {
    for e in session.events.iter().rev().take(5).collect::<Vec<_>>().into_iter().rev() {
        eprintln!("[{}] {}", e.kind, e.detail);
    }
    if let Some(ref h) = session.pending_hitl {
        println!("pending_hitl tool={} reason={}", h.tool, h.reason);
    }
}

async fn open_session(
    cfg: &Config,
    mock: bool,
    max_turns: u32,
    resume: Option<Uuid>,
    worktree: bool,
) -> anyhow::Result<AgentSession> {
    let model: Arc<dyn ModelClient> = if mock {
        Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "Mock model ready.".into(),
            tool_calls: vec![],
            usage: None,
        }]))
    } else {
        match client_from_config(cfg) {
            Ok(c) => Arc::from(c),
            Err(e) => {
                eprintln!("warning: model client: {e}; using mock");
                Arc::new(MockModelClient::script(vec![ModelResponse {
                    text: format!("(offline mock) {e}"),
                    tool_calls: vec![],
                    usage: None,
                }]))
            }
        }
    };

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

    if !cfg.mcp.servers.is_empty() {
        let mut mgr = McpManager::new();
        let errors = mgr.connect_all(&cfg.mcp.servers).await;
        for e in errors {
            eprintln!("mcp: {e}");
        }
        let _ = mgr.register_into(&mut tools).await;
    }

    let loop_cfg = LoopConfig {
        max_turns,
        workspace: cfg.workspace_root().to_path_buf(),
        journal_dir: cfg.journal_dir(),
        isolation: if worktree {
            IsolationMode::Worktree
        } else {
            IsolationMode::Off
        },
        enable_context_lifecycle: true,
        enable_governance: true,
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
