//! Forge CLI — launches the full-screen TUI by default.

use std::{io::Read, path::PathBuf};

use clap::{Args, Parser, Subcommand};

use forge_config::{is_trusted, Config, ConfigOverrides};
use forge_session::{
    open_session, resolve_journal_dir, run_headless, ApprovalPolicy, SessionTarget,
};
use forge_tui::{
    decide_launch, resume_session_items, run_setup, run_tui_with_launch, ExitCode, SetupRequest,
    SetupResult, TuiLaunch, TuiRuntimeConfig,
};
use forge_types::SessionId;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "forge",
    version,
    about = "Forge — an AI coding agent, editor, and shell in one terminal workspace.",
    // The long form used to *replace* the description with "Open the
    // full-screen TUI by default. Use --help or --version for CLI info." — so
    // `--help`, the form people reach for when they want more, said less than
    // `-h` and advised running the command they had just run.
    long_about = "Forge — an AI coding agent, editor, and shell in one terminal workspace.\n\n\
                  Running `forge` with no arguments opens the full-screen TUI in the current \
                  directory. Sessions are journalled to .forge/local/sessions in the workspace, \
                  so an interrupted one can be reopened with --resume.",
    after_help = "Run `forge` with no arguments to start. Press ? inside for keyboard shortcuts."
)]
struct Cli {
    /// Reopen a previous session. Omit the id to pick from a list.
    #[arg(long = "resume", value_name = "SESSION_ID", num_args = 0..=1)]
    resume: Option<Option<SessionId>>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Args, Debug)]
struct BenchArgs {
    /// Repository the agent is allowed to modify.
    #[arg(long, default_value = ".", value_name = "PATH")]
    workspace: PathBuf,

    /// Directory for the durable Forge session journal.
    #[arg(long, value_name = "PATH")]
    journal: Option<PathBuf>,

    /// Canonical model id, including its provider prefix when required.
    #[arg(long, value_name = "MODEL")]
    model: String,

    /// Stable provider route. If omitted, infer it from the model prefix.
    #[arg(long, value_name = "ROUTE")]
    route_id: Option<String>,

    /// Wire-level reasoning effort sent to the provider.
    #[arg(long, default_value = "max", value_name = "EFFORT")]
    effort: String,

    /// Approve every tool request. Use only with an isolated evaluation
    /// workspace.
    #[arg(long)]
    approve_all: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run one prompt through Forge without starting the terminal UI.
    Bench(BenchArgs),
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

    if let Err(message) = refuse_without_sandbox(forge_tools::sandbox::availability()) {
        eprintln!("{message}");
        std::process::exit(ExitCode::Failed.code());
    }

    let code = match run(cli).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::Failed
        }
    };
    std::process::exit(code.code());
}

/// Launch gate: a host that cannot confine does not start. Injected in tests
/// so a sandboxed CI host can still exercise the failure path.
fn refuse_without_sandbox(
    availability: Result<(), forge_tools::sandbox::Unavailable>,
) -> Result<(), String> {
    availability.map_err(|unavailable| unavailable.startup_message())
}

/// Turn `--resume` into a concrete target. A bare `--resume` means "the most
/// recent session", which requires looking one up; if there is none, fall back
/// to a new session and say so.
async fn resolve_target(
    cfg: &Config,
    resume: Option<Option<SessionId>>,
) -> anyhow::Result<(SessionTarget, Option<String>)> {
    match resume {
        Some(Some(session_id)) => Ok((SessionTarget::Resume(session_id), None)),
        Some(None) => {
            let (journal_dir, _) = resolve_journal_dir(cfg);
            let items = resume_session_items(&journal_dir, 10)
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
            match items.first() {
                Some(item) => {
                    let session_id = item
                        .id
                        .parse::<SessionId>()
                        .map_err(|e| anyhow::anyhow!(e))?;
                    Ok((SessionTarget::Resume(session_id), None))
                }
                None => Ok((
                    SessionTarget::New,
                    Some("No previous session found; creating a new session.".to_string()),
                )),
            }
        }
        None => Ok((SessionTarget::New, None)),
    }
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Some(Command::Bench(args)) => run_bench(args).await,
        None => run_tui(cli).await,
    }
}

async fn run_bench(args: BenchArgs) -> anyhow::Result<ExitCode> {
    let mut prompt = String::new();
    std::io::stdin().read_to_string(&mut prompt)?;
    if prompt.trim().is_empty() {
        anyhow::bail!("forge bench expects a non-empty prompt on stdin");
    }

    let cfg = Config::load(ConfigOverrides {
        workspace: Some(args.workspace),
        journal_path: args.journal.as_ref().map(|path| path.display().to_string()),
        ..Default::default()
    })
    .map_err(|e| anyhow::anyhow!(e))?;
    let mut cfg = cfg;
    // Config files intentionally do not select the active model. The bench
    // command takes that selection explicitly so a run is reproducible.
    cfg.model.model = args.model.clone();
    let route_id = resolve_route_id(&args.model, args.route_id.as_deref())?;

    let opened = open_session(&cfg, SessionTarget::New).await?;
    let notices = opened.notices;
    let mut session = opened.session;
    if let Some(route_id) = route_id.as_deref() {
        session.set_active_route_id(route_id);
    }
    session.set_reasoning_effort(Some(args.effort.clone()));

    let response = run_headless(
        session,
        &prompt,
        if args.approve_all {
            ApprovalPolicy::ApproveAll
        } else {
            ApprovalPolicy::Ask
        },
    )
    .await?;

    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "model": args.model,
            "route_id": route_id,
            "effort": args.effort,
            "response": response,
            "notices": notices,
        }))?
    );
    Ok(ExitCode::Success)
}

fn resolve_route_id(model: &str, explicit: Option<&str>) -> anyhow::Result<Option<String>> {
    let registry = forge_connect::loaded_registry();
    if let Some(route_id) = explicit {
        if registry.get_by_route(route_id).is_none() {
            anyhow::bail!("unknown Forge route id `{route_id}`");
        }
        return Ok(Some(route_id.to_string()));
    }

    let prefix = model
        .split_once('/')
        .map(|(prefix, _)| prefix)
        .unwrap_or(model);
    let mut matches = registry
        .profiles()
        .iter()
        .filter(|profile| {
            profile.model_provider_prefix.eq_ignore_ascii_case(prefix)
                || profile.id.eq_ignore_ascii_case(prefix)
        })
        .map(|profile| profile.route_id().to_string())
        .collect::<Vec<_>>();
    matches.sort_unstable();
    matches.dedup();
    match matches.as_slice() {
        [] => Ok(None),
        [route_id] => Ok(Some(route_id.clone())),
        _ => {
            anyhow::bail!("model prefix `{prefix}` maps to multiple Forge routes; pass --route-id")
        }
    }
}

async fn run_tui(cli: Cli) -> anyhow::Result<ExitCode> {
    let overrides = ConfigOverrides {
        config_path: None,
        workspace: None,
        model_provider: None,
        model_id: None,
        api_key: None,
        journal_path: None,
    };
    let cfg = Config::load(overrides).map_err(|e| anyhow::anyhow!(e))?;

    let cwd = cfg.workspace_root().to_path_buf();
    let trusted_at_start = is_trusted(&cwd);
    let provider_connected = forge_connect::has_connected_profile();
    let decision = decide_launch(
        cfg.tui.theme_committed,
        trusted_at_start,
        provider_connected,
    );

    if decision.run_theme_setup || decision.run_trust_setup {
        match run_setup(SetupRequest {
            run_theme: decision.run_theme_setup,
            run_trust: decision.run_trust_setup,
            cwd: cwd.clone(),
        })
        .map_err(|e| anyhow::anyhow!(e))?
        {
            SetupResult::Canceled => return Ok(ExitCode::Canceled),
            SetupResult::Completed => {}
        }
    }

    // Repository ownership comes before any session exists. The exclusive
    // lease is what enforces one Forge per repository group; acquiring it
    // after `open_session` would let a losing process write session state
    // first and only then discover it does not own the repository.
    let mut startup_notices = Vec::new();
    let bootstrap = if forge_session::RepositoryRuntimeStorage::new(&cwd).is_ok() {
        match forge_session::RepositoryBootstrap::acquire(&cfg).await {
            Ok(bootstrap) => {
                match bootstrap.recover_interrupted_creations().await {
                    Ok(notices) => startup_notices.extend(notices),
                    Err(error) => {
                        startup_notices.push(format!("task recovery incomplete: {error}"));
                    }
                }
                Some(bootstrap)
            }
            Err(error) => {
                startup_notices.push(format!("multi-task mode unavailable: {error}"));
                None
            }
        }
    } else {
        None
    };

    let (target, create_notice) = if decision.allow_resume_picker {
        resolve_target(&cfg, cli.resume).await?
    } else {
        (SessionTarget::New, None)
    };
    let opened = open_session(&cfg, target).await?;
    startup_notices.extend(opened.notices);
    if let Some(notice) = create_notice {
        startup_notices.push(notice);
    }

    let cfg = Config::load(ConfigOverrides {
        config_path: None,
        workspace: None,
        model_provider: None,
        model_id: None,
        api_key: None,
        journal_path: None,
    })
    .map_err(|e| anyhow::anyhow!(e))?;
    let last = forge_connect::PreferenceStore::user_default()
        .last_selection_struct()
        .ok()
        .flatten()
        .filter(|selection| {
            let registry = forge_connect::loaded_registry();
            selection
                .profile_id
                .as_deref()
                .and_then(|id| registry.get(id))
                .is_some()
                && forge_connect::has_connected_profile()
        });
    let supervisor = match bootstrap {
        Some(bootstrap) => match bootstrap
            .open_siblings(&cfg, opened.session.session_id)
            .await
        {
            Ok((_supervisor, handle)) => Some(handle),
            Err(error) => {
                startup_notices.push(format!("multi-task mode unavailable: {error}"));
                None
            }
        },
        None => None,
    };
    let runtime = TuiRuntimeConfig {
        model_label: last
            .as_ref()
            .map(|selection| selection.model.clone())
            .unwrap_or_default(),
        provider: last
            .as_ref()
            .map(|_| "native".to_string())
            .unwrap_or_default(),
        cwd,
        version: env!("CARGO_PKG_VERSION").into(),
        startup_notices,
        file_icons: cfg.tui.file_icons,
        theme_id: cfg.tui.theme.clone(),
    };
    let summary = run_tui_with_launch(
        opened.session,
        runtime,
        TuiLaunch {
            startup_items: None,
            onboarding_connect: decision.require_connect,
            ready_placeholder: decision.show_ready_placeholder,
            supervisor,
        },
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_resume_parses_session_id() {
        let session_id = SessionId::new_v4();
        let cli = Cli::try_parse_from(["forge", "--resume", &session_id.to_string()]).unwrap();
        assert_eq!(cli.resume, Some(Some(session_id)));
    }

    #[test]
    fn cli_resume_without_session_id_is_accepted() {
        let cli = Cli::try_parse_from(["forge", "--resume"]).unwrap();
        assert_eq!(cli.resume, Some(None));
    }

    #[test]
    fn cli_bench_parses_the_native_headless_options() {
        let cli = Cli::try_parse_from([
            "forge",
            "bench",
            "--workspace",
            "/tmp/repo",
            "--journal",
            "/tmp/journal",
            "--model",
            "openai-codex/gpt-5.6-luna",
            "--route-id",
            "openai-chatgpt",
            "--effort",
            "max",
            "--approve-all",
        ])
        .unwrap();
        let Some(Command::Bench(args)) = cli.command else {
            panic!("expected forge bench subcommand");
        };
        assert_eq!(args.workspace, PathBuf::from("/tmp/repo"));
        assert_eq!(args.journal, Some(PathBuf::from("/tmp/journal")));
        assert_eq!(args.model, "openai-codex/gpt-5.6-luna");
        assert_eq!(args.route_id.as_deref(), Some("openai-chatgpt"));
        assert_eq!(args.effort, "max");
        assert!(args.approve_all);
    }

    #[test]
    fn bench_infers_the_codex_subscription_route_from_the_model_prefix() {
        assert_eq!(
            resolve_route_id("openai-codex/gpt-5.6-luna", None).unwrap(),
            Some("openai-chatgpt".into())
        );
    }

    /// An explicit session id needs no journal lookup, so this maps straight
    /// through without touching the filesystem.
    #[tokio::test]
    async fn explicit_resume_id_maps_to_a_resume_target() {
        let session_id = SessionId::new_v4();
        let cfg = Config::default();
        let (target, notice) = resolve_target(&cfg, Some(Some(session_id))).await.unwrap();
        assert_eq!(target, SessionTarget::Resume(session_id));
        assert!(notice.is_none());
    }

    #[test]
    fn print_and_approvals_are_not_offered() {
        assert!(Cli::try_parse_from(["forge", "-p", "hi"]).is_err());
        assert!(Cli::try_parse_from(["forge", "--print", "hi"]).is_err());
        assert!(Cli::try_parse_from(["forge", "--approvals", "ask"]).is_err());
    }

    #[tokio::test]
    async fn no_resume_flag_maps_to_a_new_session() {
        let cfg = Config::default();
        let (target, notice) = resolve_target(&cfg, None).await.unwrap();
        assert_eq!(target, SessionTarget::New);
        assert!(notice.is_none());
    }

    #[test]
    fn refuse_without_sandbox_passes_when_available() {
        assert!(refuse_without_sandbox(Ok(())).is_ok());
    }

    #[test]
    fn refuse_without_sandbox_prints_the_startup_message() {
        let err = refuse_without_sandbox(Err(
            forge_tools::sandbox::Unavailable::MissingDependency("bubblewrap"),
        ))
        .expect_err("missing sandbox must refuse");
        assert!(
            err.contains("sandbox unavailable: bubblewrap not found"),
            "{err}"
        );
        assert!(err.contains("sudo apt install bubblewrap"), "{err}");
    }
}
