//! Frontend-independent session assembly.
//!
//! Building a usable `AgentSession` means resolving credentials, registering
//! MCP servers, locating the journal, and composing governance from workspace
//! permissions. That work used to live in `forge-cli`'s `main.rs`, which made
//! the terminal binary the only thing in the workspace that could produce a
//! session. Anything else wanting one — a background agent host or a test
//! harness — had to duplicate it.
//!
//! This crate sits above `forge-core` because assembly needs `forge-mcp` and
//! `forge-connect`, neither of which `forge-core` depends on and neither of
//! which belongs in it.

pub mod approvals;
pub mod auto_models;
pub mod computer_use;
mod control;
pub mod headless;
mod launch;
pub mod model_ranking;
pub mod model_report;
mod model_selection;
pub mod model_tiers;
mod project;
mod project_state;
mod projects;
mod snapshot;
pub mod subagent_models;
mod supervisor;
mod topology;
pub mod workspace_journals;

pub use forge_storage::RepositoryRuntimeStorage;

pub use control::{
    CompletedCreation, LeaseOwner, NewRepositorySession, PendingCreation, RepositoryControl,
    RepositoryLease, RepositorySession, RepositorySessionError, SessionLifecycle, StaleCreation,
    SupervisorTurnState, WorktreeOwnership,
};
pub use headless::{run_headless, ApprovalPolicy, ApprovalRequired};
pub use launch::{config_for_workspace, LaunchError, LaunchPlan, LaunchTarget, Launched};
pub use model_tiers::{prepare_model_tiers, SyncReport, TierPaths, TierTable};
pub use project::{LaunchResolution, Project, ProjectId, ProjectRegistry, RegistryError};
pub use project_state::{AttentionItem, AttentionKind, ProjectState, SessionSummary};
pub use projects::{
    new_attention, ConfigLoader, OpenProjectRef, ProjectError, ProjectNotice, ProjectService,
    ProjectServiceHandle, ProjectServiceOptions, ProjectSummary, TargetError, WorkspaceState,
};
pub use snapshot::{
    replayed_transcript, BackgroundTaskSnapshot, SessionDetailsSnapshot, SessionSnapshot,
    ShellExecutionSnapshot, TranscriptSnapshot, WaitingOn,
};
pub use supervisor::{
    QueuedPromptId, RepositoryBootstrap, RepositorySupervisor, RepositorySupervisorError,
    SessionRuntimeSnapshot, SupervisorCommand, SupervisorEvent, SupervisorHandle,
};
pub use topology::{ProjectKind, ProjectTopology, TopologyError};

use std::sync::Arc;

use forge_config::Config;
use forge_core::{AgentSession, LoopConfig};
use forge_governance::{parse_pattern_rules, Governance};
use forge_mcp::McpManager;
use forge_model::{client_from_config, ModelClient};
use forge_storage::RuntimeDataKind;
use forge_tools::ToolRegistry;
use forge_types::SessionId;

/// Which session to open: a new one, an existing one, or a fork.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionTarget {
    /// Create a fresh session.
    #[default]
    New,
    /// Resume the session with this id.
    Resume(SessionId),
    /// Create a new session seeded from this session's model-visible context.
    Fork(SessionId),
}

/// A session plus any non-fatal notices produced while assembling it
/// (storage fallbacks, refused config keys, MCP connection failures).
/// Notices are informational — the session is usable regardless.
pub struct OpenedSession {
    pub session: AgentSession,
    pub notices: Vec<String>,
}

/// Resolve where the session journal lives, and any startup notices the
/// resolution itself produced. An explicit `journal.path` override (via
/// `forge.toml`/env/CLI) is respected as-is — advanced use, not managed by
/// the storage resolver. Otherwise, route through the centralized
/// runtime-storage resolver through the project topology:
/// `.forge/local/sessions` in the main worktree of a Git repository
/// (natively excluded from `git status`) or in the root of a directory
/// project — surfacing a notice if repository-local storage fell back, or if
/// legacy runtime files were found already tracked by Git (never silently
/// migrated or altered).
pub fn resolve_journal_dir(cfg: &Config) -> (std::path::PathBuf, Vec<String>) {
    journal_dir(cfg, JournalDirFor::Writing)
}

/// What a journal directory is wanted for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JournalDirFor {
    /// A session is about to write: the directory is made ready (created,
    /// git-excluded), falling back to application data when it cannot be.
    Writing,
    /// Reading what sessions wrote: nothing is created, and the directory
    /// is wherever they put it (see `RuntimeStorage::path_for_existing`).
    Reading,
}

/// Where `cfg`'s workspace keeps its session journals, for `purpose`, and
/// any notices about it. One rule for writers and readers, so they agree.
pub(crate) fn journal_dir(
    cfg: &Config,
    purpose: JournalDirFor,
) -> (std::path::PathBuf, Vec<String>) {
    let topology = ProjectTopology::detect(cfg.workspace_root()).ok();
    if cfg.journal.path == forge_config::default_journal_path() {
        if let Some(storage) = topology.as_ref().and_then(|t| t.storage().ok()) {
            match purpose {
                JournalDirFor::Writing => {
                    let Ok(dir) = storage.path_for(RuntimeDataKind::Session) else {
                        return (cfg.journal_dir(), Vec::new());
                    };
                    let notices = storage.fallback_reason().into_iter().collect();
                    return (dir, notices);
                }
                JournalDirFor::Reading => {
                    if let Some(dir) = storage.path_for_existing(RuntimeDataKind::Session) {
                        // Why the sessions are not in `.forge/local`, when a
                        // reader can already tell (it was committed to).
                        let notices = storage.fallback_reason().into_iter().collect();
                        return (dir, notices);
                    }
                }
            }
        }
    }
    let configured = std::path::PathBuf::from(&cfg.journal.path);
    if configured.is_absolute() {
        return (configured, Vec::new());
    }
    match topology {
        Some(topology) => (topology.root().join(configured), Vec::new()),
        None => (cfg.journal_dir(), Vec::new()),
    }
}

/// The routing snapshot for a session: the loaded provider registry over
/// the process credentials handle. Credentials are read per request, so a
/// new key needs no rebuild; a definition change does (`ReloadProviders`).
pub fn route_table() -> forge_model::RouteTable {
    forge_connect::resolve(
        &forge_connect::loaded_registry(),
        &forge_connect::Credentials::shared(),
    )
}

/// The user's tier table, which resolves tiers (`fast`/`default`/`strong`)
/// in memory. Loading the table and the account model lists happens here,
/// when the session opens; building it happens in the background. Neither
/// is on the path of a turn. A table that cannot be read leaves tiers
/// changing nothing, and never stops the session opening. The table also
/// knows which models the account can run.
async fn load_model_tiers(cfg: &Config) -> Option<Arc<model_tiers::TierTable>> {
    let paths = model_tiers::TierPaths::from_config(cfg);
    let table = model_tiers::TierTable::for_paths(&paths)?;
    if let Err(error) = table.reload().await {
        tracing::warn!(%error, path = %table.path().display(), "could not read the model tier table");
    }
    if let Err(error) = table
        .refresh_runnable(&paths, std::time::Duration::from_secs(60))
        .await
    {
        tracing::warn!(%error, "could not read the account model lists for tiers");
    }
    if !cfg!(test) {
        // models.dev stale, a provider added: rebuild what changed.
        table.maintain_in_background(paths, std::time::Duration::from_secs(600));
    }
    Some(table)
}

/// The services that choose a session's models: `spawn_agent`'s model
/// check, the tier table, and the advisors that choose from the ranking of
/// this workspace's journals — a subagent's model, when it is started
/// without a tier or a model (`subagent_models`), and each turn's when the
/// user chose Auto (`auto_models`). The ranking is shared by the project's
/// sessions and built in the background: opening a session never waits for
/// it.
async fn model_services(cfg: &Config, journal_dir: &std::path::Path) -> forge_core::ModelServices {
    let models = Arc::new(model_selection::CatalogChildModels::new(cfg));
    let tiers = load_model_tiers(cfg).await;
    let (subagents, auto) = model_advisors(cfg, journal_dir, tiers.clone(), models.clone(), None);
    forge_core::ModelServices {
        child_models: Some(models),
        tiers: tiers.map(|table| {
            Arc::new(model_tiers::TableTiers(table)) as Arc<dyn forge_core::ModelTierResolver>
        }),
        subagent_advisor: Some(subagents),
        auto_advisor: Some(auto),
    }
}

/// Install [`model_advisors`] in `session`.
#[cfg(test)]
fn install_model_advisors_with(
    session: &mut AgentSession,
    cfg: &Config,
    journal_dir: &std::path::Path,
    tiers: Option<Arc<model_tiers::TierTable>>,
    models: Arc<model_selection::CatalogChildModels>,
    trust_store: Option<&std::path::Path>,
) {
    let (subagents, auto) = model_advisors(cfg, journal_dir, tiers, models, trust_store);
    session.install_model_services(forge_core::ModelServices {
        subagent_advisor: Some(subagents),
        auto_advisor: Some(auto),
        ..Default::default()
    });
}

/// The subagent and Auto advisors, reading one ranking, already warming:
/// `models` resolves models and knows their efforts, `trust_store` holds
/// the user's trust (the user's own, `None`, outside tests).
fn model_advisors(
    cfg: &Config,
    journal_dir: &std::path::Path,
    tiers: Option<Arc<model_tiers::TierTable>>,
    models: Arc<model_selection::CatalogChildModels>,
    trust_store: Option<&std::path::Path>,
) -> (
    Arc<subagent_models::RankedSubagentModels>,
    Arc<auto_models::RankedAutoModels>,
) {
    let runnable: Arc<dyn model_ranking::RunnableModels> = match tiers {
        Some(table) => table,
        None => Arc::new(model_ranking::UnknownRunnableModels),
    };
    // One source and one cache: the two advisors never build twice.
    let source: Arc<dyn model_ranking::RankingSource> =
        Arc::new(model_ranking::JournalRanking::for_workspace(
            journal_dir.to_path_buf(),
            cfg.workspace_root().to_path_buf(),
            trust_store.map(std::path::Path::to_path_buf),
        ));
    let cache = model_ranking::shared_cache(journal_dir);
    let settings = subagent_models::settings_for(cfg, trust_store);
    let auto = auto_models::RankedAutoModels::with_cache(
        source.clone(),
        runnable.clone(),
        models.clone(),
        models.clone(),
        auto_models::AutoSettings {
            trusted: settings.trusted,
            stale_after: settings.stale_after,
        },
        cache.clone(),
    );
    let subagents = subagent_models::RankedSubagentModels::with_cache(
        source, runnable, models, settings, cache,
    );
    subagents.warm();
    auto.warm();
    (subagents, auto)
}

/// The store holding the last model selection. Test builds never fall back
/// to the user's own file: a test that wants one sets `preferences_path`.
fn preference_store(cfg: &Config) -> Option<forge_connect::PreferenceStore> {
    match &cfg.model.preferences_path {
        Some(path) => Some(forge_connect::PreferenceStore::new(path.clone())),
        None if cfg!(test) => None,
        None => Some(forge_connect::PreferenceStore::user_default()),
    }
}

/// The `(model, profile id, reasoning effort)` a new session starts on:
/// `mock` for the offline client, else the last selection saved in
/// `preferences.toml` (in its current form), else none.
pub fn starting_model(cfg: &Config) -> (String, String, Option<String>) {
    if cfg.model.mock {
        return ("mock".into(), String::new(), None);
    }
    preference_store(cfg)
        .and_then(|store| {
            store
                .last_selection_struct(&forge_connect::loaded_registry())
                .ok()
                .flatten()
        })
        .map(|selection| {
            let effort = (!selection.effort.is_empty()).then_some(selection.effort);
            (
                selection.model,
                selection.profile_id.unwrap_or_default(),
                effort,
            )
        })
        .unwrap_or_default()
}

/// Save a deliberate model switch as the selection the next fresh session
/// starts on. Best-effort: a preference that fails to save must not fail the
/// switch itself.
pub(crate) fn remember_model_selection(
    cfg: &Config,
    model: &str,
    profile_id: &str,
    effort: Option<&str>,
) {
    if cfg.model.mock || model.is_empty() {
        return;
    }
    let Some(store) = preference_store(cfg) else {
        return;
    };
    let saved = store
        .set_last_selection(profile_id, model)
        .and_then(|()| store.set_last_effort(effort.unwrap_or_default()));
    if let Err(error) = saved {
        tracing::warn!("could not save the model selection: {error}");
    }
}

/// User-scope custom agents: `<user config dir>/forge/agents`, or
/// `FORGE_USER_AGENTS_DIR`. Test builds read none, as with the preference
/// store: a test that wants user agents gives its session a directory.
fn user_agents_dir() -> Option<std::path::PathBuf> {
    if cfg!(test) {
        None
    } else {
        forge_config::agents::user_agents_dir()
    }
}

/// Assemble a session from configuration, independent of any frontend.
pub async fn open_session(cfg: &Config, target: SessionTarget) -> anyhow::Result<OpenedSession> {
    let model: Arc<dyn ModelClient> =
        Arc::from(client_from_config(cfg).map_err(|e| anyhow::anyhow!(e))?);

    open_session_with_model(cfg, target, model).await
}

async fn open_session_with_model(
    cfg: &Config,
    target: SessionTarget,
    model: Arc<dyn ModelClient>,
) -> anyhow::Result<OpenedSession> {
    let mut tools = ToolRegistry::new();
    // Browser and desktop control: always registered, offered only while
    // turned on in the user's settings (which can change while it runs).
    let computer_use = computer_use::ComputerUse::global().install(&mut tools, cfg);

    let mut notices = cfg.refused_key_notices();
    // One live egress state for the whole session, built before the MCP
    // servers start (they start before the session exists) and adopted by
    // the session gateway below. Sandboxed servers route through endpoints of
    // their own on this state, so a host granted for the session — or
    // persistently — reaches their traffic immediately, and their
    // connections appear in the session event log as `mcp:<server>`.
    let egress_state = forge_tools::egress::EgressShared::new(
        forge_core::egress_policy_for_workspace(cfg.workspace_root()),
        Default::default(),
    );
    // A grant the user takes back in the settings ends here too.
    egress_state.follow_project_grants(cfg.workspace_root());
    if !cfg.mcp.servers.is_empty() {
        let mut mgr = McpManager::new().with_egress_shared(egress_state.clone());
        let errors = mgr
            .connect_all_in_workspace(&cfg.mcp.servers, cfg.workspace_root())
            .await;
        for e in errors {
            notices.push(format!("mcp: {e}"));
        }
        for e in mgr.register_into(&mut tools).await {
            notices.push(format!("mcp: {e}"));
        }
        forge_mcp::install_search_tools(&mut tools);
    }

    let (journal_dir, storage_notices) = resolve_journal_dir(cfg);
    notices.extend(storage_notices);
    let loop_cfg = LoopConfig {
        workspace: cfg.workspace_root().to_path_buf(),
        journal_dir: journal_dir.clone(),
        enable_context_lifecycle: true,
        enable_governance: true,
        agents: cfg.tools.agents.clone(),
        user_agents_dir: user_agents_dir(),
    };

    let mut session = match target {
        SessionTarget::New => AgentSession::create(loop_cfg, model, tools)
            .await
            .map_err(|e| anyhow::anyhow!(e))?,
        SessionTarget::Resume(session_id) => {
            AgentSession::resume(loop_cfg, model, tools, session_id)
                .await
                .map_err(|e| anyhow::anyhow!(e))?
        }
        SessionTarget::Fork(session_id) => {
            let source = AgentSession::resume(loop_cfg, model.clone(), tools.clone(), session_id)
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
            source.fork().await.map_err(|e| anyhow::anyhow!(e))?
        }
    };
    // A session that has no model yet starts on the last selection (a
    // resumed or forked one keeps its own).
    let (start_model, start_profile, start_effort) = starting_model(cfg);
    if session.model_selection().model.is_empty() && !start_model.is_empty() {
        let mut selection = session.model_selection();
        selection.model = start_model;
        selection.profile_id = start_profile;
        selection.reasoning_effort = start_effort;
        session.set_model_selection(selection);
    }
    // A resumed or forked session keeps its own model; either way the
    // capabilities are derived for the model the session ends up on.
    model_selection::derive_capabilities(&mut session, cfg);

    // Requests route through the snapshot, and provider keys never reach
    // model-triggered child processes: the built-in vendor aliases are
    // stripped by forge-tools, the snapshot adds every generated name and
    // every key name the loaded profiles read (custom `api_key_env` too).
    session.set_route_table(route_table());
    // A model or effort `spawn_agent` asks for is checked against the same
    // providers and catalog. The offline mock has neither.
    if !cfg.model.mock {
        session.install_model_services(model_services(cfg, &journal_dir).await);
    }
    // How long an app started with `app_start` may take to answer.
    session.set_web_ready_timeout(cfg.web.ready_timeout());
    // The project's own validation command counts as verification in the
    // turn records.
    if let Some(command) = &cfg.validation.command {
        let line = std::iter::once(command.executable.as_str())
            .chain(command.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        session.set_verification_commands(vec![line]);
    }

    // The project's own remotes are where its pushes go: record them once,
    // so only a push somewhere else asks.
    let _ = forge_config::record_project_remotes(
        cfg.workspace_root(),
        &forge_tools::remote_write_targets(cfg.workspace_root()),
    );
    let (permissions, permission_notices) = forge_config::load_permissions(cfg.workspace_root());
    notices.extend(permission_notices);
    session.set_governance(Governance::default().with_pattern_rules(
        parse_pattern_rules(&permissions.allow),
        parse_pattern_rules(&permissions.deny),
    ));
    // Serve the state the MCP servers share. It already holds the personal
    // `host(...)` policy; later `apply_egress_policy` calls replace that
    // policy in place, so the servers keep following it.
    session.adopt_egress_state(egress_state).await;
    computer_use::ComputerUse::global().register(
        session.session_id,
        computer_use,
        &session.web_app_slot(),
    );

    Ok(OpenedSession { session, notices })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    /// A model switch is what a fresh session starts on after a restart.
    #[test]
    fn a_remembered_selection_is_where_the_next_session_starts() {
        let dir = TempDir::new().unwrap();
        let mut cfg = Config::default();
        cfg.model.preferences_path = Some(dir.path().join("preferences.toml"));
        assert_eq!(starting_model(&cfg), (String::new(), String::new(), None));

        remember_model_selection(&cfg, "anthropic/claude-test", "anthropic", Some("high"));
        assert_eq!(
            starting_model(&cfg),
            (
                "anthropic/claude-test".into(),
                "anthropic".into(),
                Some("high".into())
            )
        );

        // Clearing the effort is remembered too.
        remember_model_selection(&cfg, "anthropic/claude-test", "anthropic", None);
        assert_eq!(starting_model(&cfg).2, None);
    }

    /// The offline mock client never touches the preferences file.
    #[test]
    fn the_mock_client_neither_reads_nor_writes_the_selection() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("preferences.toml");
        let mut cfg = Config::default();
        cfg.model.mock = true;
        cfg.model.preferences_path = Some(path.clone());
        remember_model_selection(&cfg, "anthropic/claude-test", "anthropic", None);
        assert!(!path.exists());
        assert_eq!(starting_model(&cfg).0, "mock");
    }

    /// Exercises the exact composition `open_session` wires at startup —
    /// `load_permissions` output feeding `Governance::with_pattern_rules` —
    /// without needing a real model client. A workspace-scope `deny` rule
    /// must actually affect `authorize()`, proving the plumbing is live, not
    /// just parsed and discarded.
    #[test]
    fn workspace_deny_rule_reaches_governance_authorize() {
        let dir = TempDir::new().unwrap();
        let forge_dir = dir.path().join(".forge");
        std::fs::create_dir_all(&forge_dir).unwrap();
        std::fs::write(
            forge_dir.join("permissions.toml"),
            "allow = [\"bash(*)\"]\ndeny = [\"bash(rm -rf*)\"]\n",
        )
        .unwrap();

        let (permissions, _notices) = forge_config::load_permissions(dir.path());
        let governance = Governance::default().with_pattern_rules(
            parse_pattern_rules(&permissions.allow),
            parse_pattern_rules(&permissions.deny),
        );

        let call = forge_types::ToolCall {
            id: "1".into(),
            name: "bash".into(),
            arguments: json!({"command": "rm -rf /"}),
        };
        assert_eq!(
            governance.authorize(&call, forge_types::SideEffectClass::Exec),
            forge_types::PolicyDecision::Hitl,
            "repo-scope allow rules must never be honored, and deny always wins anyway"
        );
    }

    /// `forge-tools` cannot see `forge-connect`, so the list of credential
    /// variables the shell tool strips is maintained by hand. This crate depends
    /// on both, so it is where the two can be checked against each other: a new
    /// provider whose key is not on that list would otherwise be readable by any
    /// model-authored command, silently.
    #[test]
    fn credential_env_names_cover_every_connect_profile() {
        let registry = forge_connect::builtin_registry();
        let stripped = forge_tools::PROVIDER_CREDENTIAL_ENV;

        for profile in registry.profiles() {
            for name in &profile.api_key_env {
                assert!(
                    stripped.contains(&name.as_str()),
                    "`{name}` (profile `{}`) is a provider credential that the shell tool would \
                     not strip — add it to forge_tools::PROVIDER_CREDENTIAL_ENV",
                    profile.id
                );
            }
        }
    }

    /// Custom profiles declare their own key names. Those cannot be in the
    /// fixed list, so the session hands them to the tool layer instead.
    #[test]
    fn custom_profile_key_names_are_stripped_from_child_processes() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(
            &path,
            r#"[[providers]]
id = "groq"
title = "Groq"
description = "Groq gateway"
route_id = "groq"
model_provider_prefix = "groq"
default_base_url = "https://api.groq.com/openai/v1"
api_key_env = ["GROQ_API_KEY"]
transport = "openai-compat"
catalog_mode = "live"
auth_mode = { api_key = { tui_always_prompt = true } }
"#,
        )
        .unwrap();
        let registry = forge_connect::loaded_registry_from(&path);
        assert!(
            registry.get("groq").is_some(),
            "fixture must load: {:?}",
            registry.load_warnings()
        );
        let names = forge_connect::credential_env_names(&registry);
        assert!(names.iter().any(|name| name == "GROQ_API_KEY"));
        assert!(!forge_tools::PROVIDER_CREDENTIAL_ENV.contains(&"GROQ_API_KEY"));
        assert!(forge_tools::is_provider_credential_env(
            "GROQ_API_KEY",
            &names
        ));
        assert!(forge_tools::is_provider_credential_env(
            "FORGE_GROQ_API_KEY",
            &[]
        ));
    }

    /// `init_repo` git-initializes a tempdir so the journal-dir resolver
    /// exercises repository-local storage hermetically, inside that tempdir —
    /// without it, an unconfigured journal path falls back to the platform
    /// application-data directory (correct real-world behavior outside a
    /// repository, but not what a test should touch on the host machine).
    use forge_test_support::init_repo;

    #[tokio::test]
    async fn open_session_with_mock_model_builds_a_session_without_notices() {
        let temp = TempDir::new().unwrap();
        init_repo(temp.path());
        let mut cfg = Config::default();
        cfg.model.mock = true;
        cfg.resolved_workspace = temp.path().to_path_buf();
        cfg.workspace_root = Some(temp.path().display().to_string());

        let opened = open_session(&cfg, SessionTarget::New).await.unwrap();
        assert!(opened.notices.is_empty());
        assert_eq!(opened.session.session_id.to_string().len(), 36);
    }

    #[test]
    fn resolve_journal_dir_uses_the_storage_resolver_for_the_unconfigured_default() {
        let temp = TempDir::new().unwrap();
        init_repo(temp.path());
        let cfg = Config {
            resolved_workspace: temp.path().to_path_buf(),
            workspace_root: Some(temp.path().display().to_string()),
            ..Default::default()
        };

        let (dir, notices) = resolve_journal_dir(&cfg);
        assert_eq!(
            dir.canonicalize().unwrap(),
            temp.path()
                .join(".forge")
                .join("local")
                .join("sessions")
                .canonicalize()
                .unwrap()
        );
        assert!(notices.is_empty());
    }

    #[test]
    fn resolve_journal_dir_respects_an_explicit_override() {
        let temp = TempDir::new().unwrap();
        init_repo(temp.path());
        let cfg = Config {
            resolved_workspace: temp.path().to_path_buf(),
            workspace_root: Some(temp.path().display().to_string()),
            journal: forge_config::JournalConfig {
                path: "custom/journal".into(),
                ..Default::default()
            },
            ..Default::default()
        };

        let (dir, notices) = resolve_journal_dir(&cfg);
        assert_eq!(
            dir,
            temp.path().canonicalize().unwrap().join("custom/journal")
        );
        assert!(notices.is_empty());
    }

    #[test]
    fn resolve_journal_dir_does_not_import_or_report_legacy_files() {
        let temp = TempDir::new().unwrap();
        init_repo(temp.path());
        std::fs::create_dir_all(temp.path().join(".forge")).unwrap();
        std::fs::write(temp.path().join(".forge/ui-state.json"), "{}").unwrap();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(temp.path())
            .args(["add", ".forge/ui-state.json"])
            .status()
            .unwrap();
        assert!(status.success());

        let cfg = Config {
            resolved_workspace: temp.path().to_path_buf(),
            workspace_root: Some(temp.path().display().to_string()),
            ..Default::default()
        };

        let (dir, notices) = resolve_journal_dir(&cfg);
        assert!(notices.is_empty());
        assert!(dir.ends_with(".forge/local/sessions"));
        assert!(temp.path().join(".forge/ui-state.json").exists());
    }

    // ------------------------------------------- the subagent model advisor

    mod subagent_model_wiring {
        use super::*;
        use forge_test_support::turn_records::TurnRecordBuilder;
        use std::collections::{HashMap, HashSet};
        use std::path::PathBuf;
        use std::time::Duration;

        const OPUS: &str = "anthropic/claude-opus-4-1";
        const SONNET: &str = "anthropic/claude-sonnet-4";
        const HELPER_PROMPT: &str = "inspect the files";

        fn git(dir: &std::path::Path, args: &[&str]) {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        }

        /// Everything user-level the wiring reads, in temporary files: the
        /// trust store, the tier table (and its list of runnable models), the
        /// catalog; plus the project's journals, holding a ranking of subagent
        /// work on anthropic where opus (12 turns, all completed) leads sonnet
        /// (10 turns, 3 failed). Efforts are the models' defaults.
        struct World {
            _dir: TempDir,
            repo: PathBuf,
            journals: PathBuf,
            store: PathBuf,
            cfg: Config,
            tiers: Arc<model_tiers::TierTable>,
        }

        async fn world(trusted: bool) -> World {
            let dir = TempDir::new().unwrap();
            let repo = dir.path().join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "-q", "--initial-branch=main"]);
            git(&repo, &["config", "user.email", "forge@example.com"]);
            git(&repo, &["config", "user.name", "Forge Test"]);
            git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
            let store = dir.path().join("trust.toml");
            if trusted {
                forge_config::grant_trust_at(&store, &repo).unwrap();
            }
            let journals = dir.path().join("journals");
            let history = forge_durable::new_session_id();
            let journal = forge_durable::Journal::open(&journals, history)
                .await
                .unwrap();
            for (kind, model, turns, failed) in [
                (forge_types::TaskKind::Subagent, OPUS, 12, 0),
                (forge_types::TaskKind::Subagent, SONNET, 10, 3),
                (forge_types::TaskKind::Main, OPUS, 12, 0),
                (forge_types::TaskKind::Main, SONNET, 10, 3),
            ] {
                for i in 0..turns {
                    journal
                        .append_turn_record(
                            &TurnRecordBuilder::new(kind.clone(), model)
                                .session_id(history)
                                .task_id(i)
                                .provider("anthropic")
                                .completed(i >= failed)
                                .escalated(i < failed)
                                .build(),
                        )
                        .await
                        .unwrap();
                }
            }
            drop(journal);
            let tiers = model_tiers::TierTable::at(&dir.path().join("model-tiers.db"));
            tiers.set_runnable(HashMap::from([(
                "anthropic".to_string(),
                HashSet::from([OPUS.to_string(), SONNET.to_string()]),
            )]));
            let mut cfg = Config {
                resolved_workspace: repo.clone(),
                workspace_root: Some(repo.display().to_string()),
                ..Default::default()
            };
            cfg.journal.path = journals.display().to_string();
            World {
                repo,
                journals,
                store,
                cfg,
                tiers,
                _dir: dir,
            }
        }

        /// The anthropic route alone, and an empty catalog: no user file.
        fn resolver(world: &World) -> Arc<model_selection::CatalogChildModels> {
            let catalog = world.journals.with_file_name("catalog.toml");
            std::fs::write(&catalog, "version = 2\n").unwrap();
            Arc::new(model_selection::CatalogChildModels::with_routes(
                Some(catalog),
                Box::new(|| {
                    forge_model::RouteTable::single(
                        forge_connect::RouteDefinition::from_spec(
                            &forge_connect::builtin::builtin_profile("anthropic"),
                        ),
                        forge_connect::Credential::Static(None),
                    )
                }),
            ))
        }

        /// Spawns a helper (no tier, no model) once, then finishes; the
        /// helper says it is done. Records the helper's requests' models.
        struct Delegating(std::sync::Mutex<Vec<String>>);

        #[async_trait::async_trait]
        impl ModelClient for Delegating {
            async fn complete(
                &self,
                request: forge_model::ModelRequest,
            ) -> Result<forge_types::ModelResponse, forge_model::ModelError> {
                let reply = |text: &str| forge_types::ModelResponse {
                    text: text.into(),
                    tool_calls: vec![],
                    usage: None,
                    thinking: None,
                };
                let helper = request.messages.iter().any(|message| {
                    message.role == forge_types::MessageRole::User
                        && message.content == HELPER_PROMPT
                });
                if helper {
                    self.0.lock().unwrap().push(request.model.clone());
                    return Ok(reply("Nothing to change."));
                }
                if request
                    .messages
                    .iter()
                    .any(|message| message.role == forge_types::MessageRole::Tool)
                {
                    return Ok(reply("Delegated."));
                }
                Ok(forge_types::ModelResponse {
                    text: String::new(),
                    tool_calls: vec![forge_types::ToolCall {
                        id: "spawn-1".into(),
                        name: "spawn_agent".into(),
                        arguments: json!({
                            "task_name": "helper",
                            "message": HELPER_PROMPT,
                            "mode": "read_only",
                        }),
                    }],
                    usage: None,
                    thinking: None,
                })
            }
        }

        async fn parent_session(world: &World, model: Arc<Delegating>) -> AgentSession {
            let mut session = AgentSession::create(
                LoopConfig {
                    workspace: world.repo.clone(),
                    journal_dir: world.journals.clone(),
                    enable_context_lifecycle: true,
                    enable_governance: true,
                    ..Default::default()
                },
                model,
                ToolRegistry::new(),
            )
            .await
            .unwrap();
            session.set_model_selection(forge_core::ModelExecutionSelection {
                model: SONNET.into(),
                profile_id: "anthropic".into(),
                reasoning_effort: None,
                thinking_enabled: true,
            });
            session
        }

        fn install(world: &World, session: &mut AgentSession) {
            install_model_advisors_with(
                session,
                &world.cfg,
                &world.journals,
                Some(world.tiers.clone()),
                resolver(world),
                Some(&world.store),
            );
        }

        async fn ranking_built(journals: &std::path::Path) {
            for _ in 0..500 {
                if model_ranking::shared_cache(journals).is_built() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("the ranking was never built");
        }

        /// Delegate once; the spawn's result and the helper's models.
        async fn delegate(
            session: &mut AgentSession,
            model: &Delegating,
        ) -> (serde_json::Value, Vec<String>) {
            session
                .run_user_message_with_stream("have a helper look", None)
                .await
                .unwrap();
            let result = session
                .messages
                .iter()
                .find(|message| message.tool_call_id.as_deref() == Some("spawn-1"))
                .map(|message| message.content.clone())
                .expect("a spawn_agent result");
            let task = session.background().list().next().expect("a helper").id;
            for _ in 0..500 {
                session.poll_background_tasks().await.unwrap();
                if session
                    .background()
                    .get(task)
                    .is_some_and(|task| task.status.is_terminal())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let models = model.0.lock().unwrap().clone();
            assert!(!models.is_empty(), "the helper never ran: {result}");
            (serde_json::from_str(&result).unwrap(), models)
        }

        /// Installed as a session opens: the ranking is built without any
        /// spawn asking, and a helper then runs on the leader — through the
        /// real runnable list, resolver, journal ranking and shared cache —
        /// unless this spawn is one of the about one in ten that explore.
        #[tokio::test]
        async fn in_a_trusted_workspace_a_helper_runs_on_the_ranked_leader() {
            let world = world(true).await;
            let model = Arc::new(Delegating(Default::default()));
            let mut session = parent_session(&world, model.clone()).await;
            install(&world, &mut session);
            ranking_built(&world.journals).await;

            let (result, models) = delegate(&mut session, &model).await;
            // Production settings explore about one spawn in ten: then the
            // helper keeps the parent's model, and says which it passed over.
            let choice = &result["model_choice"];
            eprintln!("wiring outcome: {}", choice["source"]);
            match choice["source"].as_str() {
                Some("ranking") => assert!(models.iter().all(|m| m == OPUS), "{models:?}"),
                Some("exploration") => {
                    assert!(models.iter().all(|m| m == SONNET), "{models:?}");
                    let reason = choice["reason"].as_str().unwrap();
                    assert!(reason.contains(OPUS), "{reason}");
                }
                _ => panic!("neither a ranked pick nor an exploration: {result}"),
            }
        }

        #[tokio::test]
        async fn in_an_untrusted_workspace_a_helper_keeps_the_parents_model() {
            let world = world(false).await;
            let model = Arc::new(Delegating(Default::default()));
            let mut session = parent_session(&world, model.clone()).await;
            install(&world, &mut session);

            let (result, models) = delegate(&mut session, &model).await;
            assert!(models.iter().all(|m| m == SONNET), "{models:?}");
            assert_eq!(result["model_choice"]["source"], "fallback");
            let reason = result["model_choice"]["reason"].as_str().unwrap();
            assert!(reason.contains("not trusted"), "{reason}");
            assert!(!model_ranking::shared_cache(&world.journals).is_built());
        }

        #[tokio::test]
        async fn the_config_switch_reaches_the_advisor() {
            let mut world = world(true).await;
            world.cfg.subagents.model_selection = forge_config::SubagentModelSelection::Inherit;
            let model = Arc::new(Delegating(Default::default()));
            let mut session = parent_session(&world, model.clone()).await;
            install(&world, &mut session);

            let (result, models) = delegate(&mut session, &model).await;
            assert!(models.iter().all(|m| m == SONNET), "{models:?}");
            let reason = result["model_choice"]["reason"].as_str().unwrap();
            assert!(reason.contains("inherit"), "{reason}");
        }

        /// Answers every request; records each request's model.
        struct Recording(std::sync::Mutex<Vec<String>>);

        #[async_trait::async_trait]
        impl ModelClient for Recording {
            async fn complete(
                &self,
                request: forge_model::ModelRequest,
            ) -> Result<forge_types::ModelResponse, forge_model::ModelError> {
                self.0.lock().unwrap().push(request.model.clone());
                Ok(forge_types::ModelResponse {
                    text: "Done.".into(),
                    tool_calls: vec![],
                    usage: None,
                    thinking: None,
                })
            }
        }

        /// A session on Auto starts its turn on the ranked leader of
        /// main-session work — through the real runnable list, resolver,
        /// journal ranking and shared cache — and says so.
        #[tokio::test]
        async fn an_auto_session_starts_its_turn_on_the_ranked_leader() {
            let world = world(true).await;
            let model = Arc::new(Recording(Default::default()));
            let mut session = AgentSession::create(
                LoopConfig {
                    workspace: world.repo.clone(),
                    journal_dir: world.journals.clone(),
                    enable_context_lifecycle: true,
                    enable_governance: true,
                    ..Default::default()
                },
                model.clone(),
                ToolRegistry::new(),
            )
            .await
            .unwrap();
            install(&world, &mut session);
            ranking_built(&world.journals).await;
            session
                .set_auto_model(forge_core::AutoFallback {
                    model: SONNET.into(),
                    profile_id: "anthropic".into(),
                    reasoning_effort: None,
                })
                .await
                .unwrap();
            session
                .run_user_message_with_stream("hello", None)
                .await
                .unwrap();
            assert_eq!(
                model.0.lock().unwrap().first().map(String::as_str),
                Some(OPUS)
            );
            let decision = &session.auto_decisions()[0];
            assert_eq!(decision.source, forge_core::AutoSource::Ranked);
            assert_eq!(decision.model, OPUS);
        }

        /// The project's sessions share one ranking: a second session does
        /// not rebuild a fresh one.
        #[tokio::test]
        async fn a_projects_sessions_share_one_ranking() {
            let world = world(true).await;
            let mut first = parent_session(&world, Arc::new(Delegating(Default::default()))).await;
            install(&world, &mut first);
            ranking_built(&world.journals).await;
            let cache = model_ranking::shared_cache(&world.journals);
            let built_at = cache.built_at();

            let mut second = parent_session(&world, Arc::new(Delegating(Default::default()))).await;
            install(&world, &mut second);
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(Arc::ptr_eq(
                &cache,
                &model_ranking::shared_cache(&world.journals)
            ));
            assert_eq!(cache.built_at(), built_at, "a fresh ranking is not rebuilt");
        }
    }
}
