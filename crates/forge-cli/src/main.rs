//! Forge CLI — launches the full-screen TUI by default.

use clap::Parser;

use forge_config::{is_trusted, Config, ConfigOverrides};
use forge_session::{open_session, resolve_journal_dir, SessionTarget};
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

    let (target, create_notice) = if decision.allow_resume_picker {
        resolve_target(&cfg, cli.resume).await?
    } else {
        (SessionTarget::New, None)
    };
    let opened = open_session(&cfg, target).await?;
    let mut startup_notices = opened.notices;
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
