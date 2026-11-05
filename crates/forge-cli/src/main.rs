//! Forge CLI — launches the full-screen TUI by default.

mod egress_connect;

use std::{io::Read, path::PathBuf, time::Duration};

use clap::{Args, Parser, Subcommand};

use forge_config::{is_trusted, Config, ConfigOverrides};
use forge_session::{
    open_session, resolve_journal_dir, run_headless, ApprovalPolicy, ApprovalRequired,
    SessionTarget,
};
use forge_tui::{
    decide_launch, resume_session_items, run_setup, run_tui_supervised, run_tui_with_launch,
    ExitCode, SetupRequest, SetupResult, TuiLaunch, TuiRuntimeConfig,
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
                  so an interrupted one can be reopened with --continue or --resume.",
    after_help = "Run `forge` with no arguments to start. Press ? inside for keyboard shortcuts."
)]
struct Cli {
    /// Reopen a previous session. Omit the id to pick from a list.
    #[arg(long = "resume", value_name = "SESSION_ID", num_args = 0..=1)]
    resume: Option<Option<SessionId>>,

    /// Continue the most recent previous session in this workspace.
    #[arg(long, conflicts_with_all = ["resume", "fork"])]
    r#continue: bool,

    /// Fork a previous session into a new conversation.
    #[arg(long, value_name = "SESSION_ID", conflicts_with_all = ["resume", "continue"])]
    fork: Option<SessionId>,

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

    /// Model id as `<provider>/<model>`, e.g. `openai_codex/gpt-5.6-luna`.
    /// The prefix is the provider profile.
    #[arg(long, value_name = "MODEL")]
    model: String,

    /// Wire-level reasoning effort sent to the provider.
    #[arg(long, default_value = "max", value_name = "EFFORT")]
    effort: String,

    /// Approve tool prompts and grant blocked network hosts for the session.
    /// Commands stay sandboxed: a request to run outside the sandbox is
    /// refused. Use only with an isolated evaluation workspace.
    #[arg(long)]
    approve_all: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run one prompt through Forge without starting the terminal UI.
    Bench(BenchArgs),
}

fn main() {
    // Internal helpers run before clap so they stay out of `--help` and
    // skip the logging and runtime setup the TUI needs.
    let mut args = std::env::args_os().skip(1);
    if args
        .next()
        .is_some_and(|arg| arg == egress_connect::SUBCOMMAND)
    {
        let rest: Vec<String> = args.map(|arg| arg.to_string_lossy().into_owned()).collect();
        std::process::exit(egress_connect::run(&rest));
    }

    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("error")),
        )
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_ansi(false)
        .init();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build Tokio runtime");
    let code = runtime.block_on(async move {
        match run(cli).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::Failed
            }
        }
    });
    runtime.shutdown_timeout(Duration::ZERO);
    std::process::exit(code.code());
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

/// How long quitting waits for the supervisor to stop its sessions and
/// release the repository lease before exiting regardless.
const SUPERVISOR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Run a supervisor shutdown, giving up after `timeout`. Returns what to
/// report on stderr, if anything; it never changes the exit code.
async fn bounded_shutdown<E: std::fmt::Display>(
    shutdown: impl std::future::Future<Output = Result<(), E>>,
    timeout: Duration,
) -> Option<String> {
    match tokio::time::timeout(timeout, shutdown).await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(format!("shutdown: {error}")),
        Err(_) => Some(format!(
            "shutdown: sessions did not stop within {}s; exiting anyway",
            timeout.as_secs()
        )),
    }
}

/// Take repository ownership for the TUI, or explain why it runs without it.
///
/// In a git repository, any failure to take ownership is fatal. Another
/// Forge owning it (`AlreadyOwned`) is the common case, but an unreadable
/// lock file or a control database the owner holds locked are no safer:
/// falling back to a lease-less session there could journal into the same
/// sessions directory as a live owner, a second writer. Only a workspace
/// with no repository runtime storage (not a git repository) runs
/// lease-less.
async fn acquire_repository(
    cfg: &Config,
    cwd: &std::path::Path,
    startup_notices: &mut Vec<String>,
) -> anyhow::Result<Option<forge_session::RepositoryBootstrap>> {
    let Ok(storage) = forge_session::RepositoryRuntimeStorage::new(cwd) else {
        return Ok(None);
    };
    match forge_session::RepositoryBootstrap::acquire(cfg).await {
        Ok(bootstrap) => {
            match bootstrap.recover_interrupted_creations().await {
                Ok(notices) => startup_notices.extend(notices),
                Err(error) => {
                    startup_notices.push(format!("Session recovery incomplete: {error}"));
                }
            }
            Ok(Some(bootstrap))
        }
        Err(
            error @ forge_session::RepositorySupervisorError::Control(
                forge_session::RepositorySessionError::AlreadyOwned { .. },
            ),
        ) => Err(error.into()),
        Err(error) => Err(anyhow::anyhow!(
            "could not take ownership of this repository ({}): {error}. \
             Fix the permissions or contents of that directory (or move it aside), \
             then start Forge again; Forge will not run a second, unowned session beside it.",
            storage
                .main_worktree()
                .join(".forge")
                .join("local")
                .display()
        )),
    }
}

fn config_for_workspace(cfg: &Config, workspace: &std::path::Path) -> Config {
    let mut bound = cfg.clone();
    bound.resolved_workspace = workspace.to_path_buf();
    bound.workspace_root = Some(workspace.display().to_string());
    let (journal_dir, _) = resolve_journal_dir(&bound);
    bound.journal.path = journal_dir.display().to_string();
    bound
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
    // Config never selects the model. The bench command takes it explicitly
    // so a run is reproducible.
    let profile_id = resolve_profile_id(&args.model)?;

    let opened = open_session(&cfg, SessionTarget::New).await?;
    let notices = opened.notices;
    let mut session = opened.session;
    session.set_active_model(args.model.clone());
    session.set_active_profile_id(profile_id.clone());
    session.set_reasoning_effort(Some(args.effort.clone()));

    let response = match run_headless(
        session,
        &prompt,
        if args.approve_all {
            ApprovalPolicy::ApproveAll
        } else {
            ApprovalPolicy::Ask
        },
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            let Some(approval) = error.downcast_ref::<ApprovalRequired>() else {
                return Err(error);
            };
            let (kind, retry_scope) = match approval.payload.kind() {
                forge_types::ApprovalKind::NetworkHost => (
                    "network_host_denial",
                    "host_grant_stays_filesystem_sandboxed",
                ),
                forge_types::ApprovalKind::SandboxEscalation => (
                    "sandbox_filesystem_denial",
                    "approved_command_retries_unconfined",
                ),
                forge_types::ApprovalKind::Governance => (
                    "governance_approval",
                    "tool_call_requires_operator_approval",
                ),
            };
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "status": "approval_required",
                    "policy": "sandboxed",
                    "approval": {
                        "kind": kind,
                        "tool": approval.payload.tool,
                        "args_redacted": approval.payload.args_redacted,
                        "reason": approval.payload.reason,
                        "failure": approval.payload.failure,
                        "denied_host": approval.payload.denied_host,
                        "sandbox_escalation": approval.payload.sandbox_escalation,
                        "retry_scope": retry_scope,
                    },
                }))?
            );
            return Ok(ExitCode::Failed);
        }
    };

    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "model": args.model,
            "profile_id": profile_id,
            "effort": args.effort,
            "response": response,
            "notices": notices,
        }))?
    );
    Ok(ExitCode::Success)
}

/// The provider profile a `<provider>/<model>` id names. An old prefix
/// (`openai-codex/…`) is refused with the id to use instead.
fn resolve_profile_id(model: &str) -> anyhow::Result<String> {
    if let Some(current) = forge_connect::old_prefix_hint(model) {
        anyhow::bail!("model `{model}` uses an old provider prefix; use `{current}`");
    }
    let Some((prefix, _)) = model.split_once('/') else {
        anyhow::bail!("model `{model}` needs a provider prefix: `<provider>/<model>`");
    };
    let registry = forge_connect::loaded_registry();
    match registry.get(prefix) {
        Some(profile) => Ok(profile.id.clone()),
        None => anyhow::bail!(
            "no provider `{prefix}`; known providers: {}",
            registry.ids().join(", ")
        ),
    }
}

async fn run_tui(cli: Cli) -> anyhow::Result<ExitCode> {
    let overrides = ConfigOverrides {
        config_path: None,
        workspace: None,
        journal_path: None,
    };
    let cfg = Config::load(overrides).map_err(|e| anyhow::anyhow!(e))?;

    let cwd = cfg.workspace_root().to_path_buf();
    let trusted_at_start = is_trusted(&cwd);
    // One provider registry for the launch decisions below.
    let registry = forge_connect::loaded_registry();
    let provider_connected = forge_connect::has_connected_profile(&registry);
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
    let bootstrap = acquire_repository(&cfg, &cwd, &mut startup_notices).await?;

    let (target, create_notice) = if decision.allow_resume_picker {
        if let Some(session_id) = cli.fork {
            (SessionTarget::Fork(session_id), None)
        } else if cli.r#continue {
            resolve_target(&cfg, Some(None)).await?
        } else {
            resolve_target(&cfg, cli.resume).await?
        }
    } else {
        (SessionTarget::New, None)
    };
    let mut open_target = target;
    let mut fork_from = None;
    let session_cfg = if let Some(bootstrap) = bootstrap.as_ref() {
        match target {
            SessionTarget::Resume(session_id) => {
                let record = bootstrap.session(session_id).await?;
                config_for_workspace(&cfg, &record.workspace)
            }
            SessionTarget::Fork(session_id) => {
                let record = bootstrap.session(session_id).await?;
                fork_from = Some(session_id);
                open_target = SessionTarget::Resume(session_id);
                config_for_workspace(&cfg, &record.workspace)
            }
            SessionTarget::New => cfg.clone(),
        }
    } else {
        cfg.clone()
    };
    let opened = open_session(&session_cfg, open_target).await?;
    let opened_session_id = opened.session.session_id;
    let runtime_cwd = opened.session.workspace_root().to_path_buf();
    startup_notices.extend(opened.notices);
    if let Some(notice) = create_notice {
        startup_notices.push(notice);
    }

    let cfg = Config::load(ConfigOverrides {
        config_path: None,
        workspace: None,
        journal_path: None,
    })
    .map_err(|e| anyhow::anyhow!(e))?;
    let last = forge_connect::PreferenceStore::user_default()
        .last_selection_struct(&registry)
        .ok()
        .flatten()
        .filter(|selection| {
            selection
                .profile_id
                .as_deref()
                .and_then(|id| registry.get(id))
                .is_some()
                && provider_connected
        });
    forge_tui::notify::install(cfg.tui.notify);
    forge_tui::git_prefs::install(cfg.tui.smart_commit);
    let runtime = TuiRuntimeConfig {
        model_label: last
            .as_ref()
            .map(|selection| selection.model.clone())
            .unwrap_or_default(),
        provider: last
            .as_ref()
            .map(|_| "native".to_string())
            .unwrap_or_default(),
        cwd: runtime_cwd,
        version: env!("CARGO_PKG_VERSION").into(),
        startup_notices,
        file_icons: cfg.tui.file_icons,
        theme_id: cfg.tui.theme.clone(),
        reduced_motion: cfg.tui.reduced_motion,
    };
    let launch = TuiLaunch {
        startup_items: None,
        onboarding_connect: decision.require_connect,
        ready_placeholder: decision.show_ready_placeholder,
    };
    let summary = match bootstrap {
        Some(bootstrap) => {
            let (supervisor, handle) = bootstrap.open_with_primary(&cfg, opened.session).await?;
            if let Some(source_session_id) = fork_from {
                handle
                    .command(forge_session::SupervisorCommand::ForkSession {
                        session_id: source_session_id,
                    })
                    .await?;
            }
            let session_id = if fork_from.is_some() {
                supervisor
                    .selected_session_id()
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("forked session was not selected"))?
            } else {
                opened_session_id
            };
            let initial = supervisor
                .snapshot(session_id)
                .await
                .ok_or_else(|| anyhow::anyhow!("primary session missing from supervisor"))?;
            let summary = run_tui_supervised(initial, runtime, handle, launch).await;
            // Release the repository explicitly rather than relying on
            // process exit, so the lease is provably free once we return.
            // Bounded: a provider that never answers must not keep the
            // terminal from quitting; process exit then frees the lease.
            if let Some(problem) =
                bounded_shutdown(supervisor.shutdown(), SUPERVISOR_SHUTDOWN_TIMEOUT).await
            {
                eprintln!("{problem}");
            }
            summary
        }
        None => run_tui_with_launch(opened.session, runtime, launch).await,
    }
    .map_err(|e| anyhow::anyhow!(e))?;
    if let Some(quit_failure) = &summary.quit_failure {
        eprintln!("quit all sessions: {quit_failure}");
    }
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
    fn cli_continue_and_fork_parse() {
        let cli = Cli::try_parse_from(["forge", "--continue"]).unwrap();
        assert!(cli.r#continue);
        let session_id = SessionId::new_v4();
        let cli = Cli::try_parse_from(["forge", "--fork", &session_id.to_string()]).unwrap();
        assert_eq!(cli.fork, Some(session_id));
    }

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
            "openai_codex/gpt-5.6-luna",
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
        assert_eq!(args.model, "openai_codex/gpt-5.6-luna");
        assert_eq!(args.effort, "max");
        assert!(args.approve_all);
    }

    #[test]
    fn bench_takes_the_profile_from_the_model_prefix() {
        assert_eq!(
            resolve_profile_id("openai_codex/gpt-5.6-luna").unwrap(),
            "openai_codex"
        );
        let old = resolve_profile_id("openai-codex/gpt-5.6-luna").unwrap_err();
        assert!(
            old.to_string().contains("openai_codex/gpt-5.6-luna"),
            "{old}"
        );
        assert!(resolve_profile_id("gpt-5").is_err());
        assert!(resolve_profile_id("nobody/model").is_err());
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

    fn git_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("tracked.txt"), "one\n").unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "forge@example.test"][..],
            &["config", "user.name", "Forge Test"][..],
            &["add", "tracked.txt"][..],
            &["commit", "-q", "-m", "init"][..],
        ] {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(temp.path())
                .args(args)
                .status()
                .unwrap()
                .success());
        }
        temp
    }

    fn workspace_config(workspace: &std::path::Path) -> Config {
        let mut cfg = Config {
            resolved_workspace: workspace.to_path_buf(),
            workspace_root: Some(workspace.display().to_string()),
            ..Default::default()
        };
        cfg.journal.path = workspace.join("journals").display().to_string();
        cfg
    }

    /// Every file under `dir`, with its bytes, for before/after comparisons.
    fn snapshot(dir: &std::path::Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        let mut files = std::collections::BTreeMap::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(next) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&next) else {
                continue;
            };
            for entry in entries {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    files.insert(path.clone(), std::fs::read(&path).unwrap_or_default());
                }
            }
        }
        files
    }

    fn find_file(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
        snapshot(dir)
            .into_keys()
            .find(|path| path.file_name().is_some_and(|file| file == name))
    }

    const LEASE_HOLDER_ENV: &str = "FORGE_TEST_LEASE_HOLDER_WORKSPACE";

    /// Child half of the two-process test: when the parent sets
    /// `LEASE_HOLDER_ENV`, take the repository lease, say so on stdout and
    /// hold it until stdin closes. Without the variable it does nothing.
    #[tokio::test]
    async fn lease_holder_child() {
        let Ok(workspace) = std::env::var(LEASE_HOLDER_ENV) else {
            return;
        };
        let cfg = workspace_config(std::path::Path::new(&workspace));
        let _owner = forge_session::RepositoryBootstrap::acquire(&cfg)
            .await
            .expect("child takes the lease");
        println!("LEASE_HELD {}", std::process::id());
        let mut sink = String::new();
        let _ = std::io::stdin().read_to_string(&mut sink);
    }

    #[tokio::test]
    async fn second_process_on_an_owned_repository_is_refused_naming_the_owner() {
        use std::io::BufRead;

        let repo = git_repo();
        let cfg = workspace_config(repo.path());
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::lease_holder_child", "--nocapture"])
            .env(LEASE_HOLDER_ENV, repo.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let child_pid = loop {
            let line = lines
                .next()
                .expect("child exited before taking the lease")
                .unwrap();
            if let Some(pid) = line.strip_prefix("LEASE_HELD ") {
                break pid.trim().to_string();
            }
        };
        assert_eq!(child_pid, child.id().to_string());
        let control = repo.path().join(".forge");
        let before = snapshot(&control);

        let mut notices = Vec::new();
        let error = acquire_repository(&cfg, repo.path(), &mut notices)
            .await
            .err()
            .expect("an owned repository must refuse, not fall back to a lease-less session");
        let message = format!("{error:#}");
        // Compare while the owner is still running: its own exit legitimately
        // touches the control database.
        let after = snapshot(&control);

        drop(child.stdin.take());
        child.wait().unwrap();
        assert!(
            message.contains("already owns this repository"),
            "refusal must name the ownership conflict: {message}"
        );
        assert!(
            message.contains(&format!("pid {child_pid},")),
            "refusal must name the owning process {child_pid}: {message}"
        );
        assert!(
            !message.contains(&format!("pid {},", std::process::id())),
            "refusal must not name this process: {message}"
        );
        assert!(
            notices.is_empty(),
            "no lease-less startup notice: {notices:?}"
        );
        let changed: Vec<_> = before
            .keys()
            .chain(after.keys())
            .filter(|path| before.get(*path) != after.get(*path))
            .collect();
        assert!(
            changed.is_empty(),
            "a refused launch must not change the owner's control directory: {changed:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unreadable_lock_file_in_a_repository_is_refused_not_lease_less() {
        use std::os::unix::fs::PermissionsExt;

        let repo = git_repo();
        let cfg = workspace_config(repo.path());
        let owner = forge_session::RepositoryBootstrap::acquire(&cfg)
            .await
            .expect("first owner takes the lease");
        let lock = find_file(&repo.path().join(".forge"), "owner.lock").expect("owner.lock");
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&lock).is_ok() {
            // Running as root: permissions do not apply, nothing to test.
            std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
            return;
        }

        let mut notices = Vec::new();
        let result = acquire_repository(&cfg, repo.path(), &mut notices).await;
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = result
            .err()
            .expect("an ownership failure other than AlreadyOwned must also refuse");
        let message = format!("{error:#}");
        assert!(message.contains("could not take ownership"), "{message}");
        assert!(
            message.contains(".forge/local"),
            "the refusal must name where the problem is: {message}"
        );
        assert!(
            notices.is_empty(),
            "no lease-less startup notice: {notices:?}"
        );
        drop(owner);
    }

    #[tokio::test]
    async fn bounded_shutdown_gives_up_after_the_timeout() {
        let problem = bounded_shutdown(
            std::future::pending::<Result<(), String>>(),
            Duration::from_millis(10),
        )
        .await
        .expect("a shutdown that never finishes must be reported");
        assert!(problem.contains("did not stop"), "{problem}");
        assert_eq!(
            bounded_shutdown(async { Ok::<(), String>(()) }, Duration::from_secs(10)).await,
            None
        );
        assert_eq!(
            bounded_shutdown(async { Err::<(), _>("boom") }, Duration::from_secs(10)).await,
            Some("shutdown: boom".to_string())
        );
    }

    #[tokio::test]
    async fn free_repository_is_acquired() {
        let repo = git_repo();
        let cfg = workspace_config(repo.path());
        let mut notices = Vec::new();
        let bootstrap = acquire_repository(&cfg, repo.path(), &mut notices)
            .await
            .unwrap();
        assert!(bootstrap.is_some());
    }

    #[tokio::test]
    async fn non_git_workspace_still_runs_without_a_lease() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = workspace_config(dir.path());
        let mut notices = Vec::new();
        let bootstrap = acquire_repository(&cfg, dir.path(), &mut notices)
            .await
            .unwrap();
        assert!(bootstrap.is_none());
        assert!(notices.is_empty());
    }
}
