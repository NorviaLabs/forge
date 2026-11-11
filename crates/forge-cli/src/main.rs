//! Forge CLI. A bare `forge` opens the desktop app; `forge bench` runs one
//! prompt headless for evaluations. `__egress-connect` is an internal helper
//! git's SSH routing invokes on this executable.

mod launch;

use std::{io::Read, path::PathBuf, time::Duration};

use clap::{Args, Parser, Subcommand};

use forge_config::{Config, ConfigOverrides};
use forge_session::{open_session, run_headless, ApprovalPolicy, ApprovalRequired, SessionTarget};
use tracing_subscriber::EnvFilter;

/// Process exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
enum ExitCode {
    Success = 0,
    Failed = 1,
}

impl ExitCode {
    fn code(self) -> i32 {
        self as i32
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "forge",
    version,
    about = "Forge — an AI coding agent for the desktop.",
    long_about = "Forge — an AI coding agent for the desktop.\n\n\
                  Running `forge` opens the desktop app in the current directory, or in PATH \
                  when given. `forge bench` runs one prompt without a window."
)]
struct Cli {
    /// Folder to open in the desktop app. Defaults to the current directory.
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,

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
    /// Run one prompt through Forge without opening a window.
    Bench(BenchArgs),
}

fn main() {
    // Internal helpers run before clap so they stay out of `--help`.
    let mut args = std::env::args_os().skip(1);
    let helper = args.next();
    let rest = || -> Vec<String> { args.map(|arg| arg.to_string_lossy().into_owned()).collect() };
    if helper
        .as_ref()
        .is_some_and(|arg| arg == forge_tools::egress_connect::SUBCOMMAND)
    {
        std::process::exit(forge_tools::egress_connect::run(&rest()));
    }
    // A confined git's ssh: forwards the session to the broker in the
    // running Forge, which holds the keys.
    if helper
        .as_ref()
        .is_some_and(|arg| arg == forge_tools::ssh_broker::SUBCOMMAND)
    {
        std::process::exit(forge_tools::ssh_broker::run(&rest()));
    }
    // A confined git's signing program: the broker in the running Forge
    // signs with the user's key, which stays outside the sandbox.
    if helper
        .as_ref()
        .is_some_and(|arg| arg == forge_tools::sign_broker::SUBCOMMAND)
    {
        std::process::exit(forge_tools::sign_broker::run(&rest()));
    }

    let cli = Cli::parse();
    let Some(Command::Bench(args)) = cli.command else {
        if !launch::display_available() {
            eprintln!("No display available; the Forge desktop app needs a desktop session.");
            std::process::exit(1);
        }
        std::process::exit(launch::run(cli.path.as_deref()));
    };

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
        match run_bench(args).await {
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn print_and_approvals_are_not_offered() {
        assert!(Cli::try_parse_from(["forge", "-p", "hi"]).is_err());
        assert!(Cli::try_parse_from(["forge", "--print", "hi"]).is_err());
        assert!(Cli::try_parse_from(["forge", "--approvals", "ask"]).is_err());
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

        let error = forge_session::LaunchPlan::prepare(&cfg)
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

        // The read-only probe in `prepare` cannot read the lock either and
        // defers to the real acquisition, which runs after trust.
        let trust = tempfile::tempdir().unwrap();
        let store = trust.path().join("trust.toml");
        forge_config::grant_trust_at(&store, repo.path()).unwrap();
        let result =
            match forge_session::LaunchPlan::prepare_with_trust_store(&cfg, Some(store)).await {
                Ok(plan) => plan
                    .launch(forge_session::LaunchTarget::New)
                    .await
                    .map(|_| ()),
                Err(error) => Err(error),
            };
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error =
            result.expect_err("an ownership failure other than AlreadyOwned must also refuse");
        let message = format!("{error:#}");
        assert!(message.contains("could not take ownership"), "{message}");
        assert!(
            message.contains(".forge/local"),
            "the refusal must name where the problem is: {message}"
        );
        assert!(
            !repo.path().join("journals").exists(),
            "no lease-less session was opened"
        );
        drop(owner);
    }
}
