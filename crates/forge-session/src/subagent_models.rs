//! Choosing the model of a subagent started without a tier or a model, from
//! the ranking of models for its kind of task (`model_report`).
//!
//! - **Candidates** are the models on the parent's provider that the account
//!   can run; the kind (`subagent`, or a custom agent's own
//!   `custom:<name>`, never borrowed from another) is ranked over those
//!   alone, from the turns of the last [`WINDOW_DAYS`] days.
//! - **Enough evidence** is at least [`MIN_KIND_TURNS`] such turns and at
//!   least [`MIN_RANKED_GROUPS`] candidates with enough turns to be ranked.
//!   The leader is then picked — model and effort — if the child-model
//!   resolver accepts it; else the next ranked candidate.
//! - **Otherwise** — thin evidence, the switch set to `inherit`, a workspace
//!   the user does not trust (its journals could have come from anyone, and
//!   are not read), no ranking built yet — the child keeps the parent's
//!   model, as before.
//! - **Exploration**: about one spawn in ten keeps the parent's model even
//!   with enough evidence, so every model keeps being tried.
//! - **Never waiting**: the ranking is built in the background and rebuilt
//!   when older than [`STALE_AFTER`]; a spawn uses what is there.
//!
//! Every answer carries its reason (`forge_core::ModelChoice`).

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::{Duration, Instant};

use chrono::Utc;
use forge_config::{Config, SubagentModelSelection};
use forge_core::{
    ChildModelRequest, ChildModelResolver, ModelAdvice, ModelAdviceRequest, ModelChoice,
    ModelChoiceEvidence, ModelChoiceSource, RankedCandidate, SubagentModelAdvisor,
};
use forge_types::TaskKind;

use crate::model_report::{
    list_session_journals, read_journals, Aggregator, GroupRef, RankedModel, RankingSnapshot,
    ReportOptions, DEFAULT_MIN_TURNS,
};

/// How far back the turns a choice is based on go.
pub const WINDOW_DAYS: u32 = 90;
/// Turns of the kind, on candidate models, needed to choose by ranking.
pub const MIN_KIND_TURNS: usize = 20;
/// Ranked candidates needed: a ranking of one compares nothing.
pub const MIN_RANKED_GROUPS: usize = 2;
/// A ranking older than this is rebuilt (in the background) when used.
pub const STALE_AFTER: Duration = Duration::from_secs(60);
/// About one spawn in this many keeps the parent's model anyway.
pub const EXPLORE_ONE_IN: u32 = 10;

/// A snapshot being built.
pub type SnapshotFuture = Pin<Box<dyn Future<Output = anyhow::Result<RankingSnapshot>> + Send>>;

/// Where the ranking comes from.
pub trait RankingSource: Send + Sync + 'static {
    fn build(&self) -> SnapshotFuture;
}

/// How often a [`JournalRanking`] reads every journal again, however few
/// changed: what moves the window (turns that have aged out drop out).
pub const FULL_REBUILD_EVERY: Duration = Duration::from_secs(3600);

/// The journals of a workspace: every session journal in the directory, the
/// last [`WINDOW_DAYS`] days of turns. Incremental: each journal's totals
/// are kept with its file stamp, and a build reads again only the journals
/// that changed since the last — a workspace's history is mostly idle
/// journals — and every journal at most every [`FULL_REBUILD_EVERY`].
pub struct JournalRanking {
    journal_dir: PathBuf,
    /// The workspace whose trust every build checks again, and the trust
    /// store (`None`: the user's own).
    trust: Option<(PathBuf, Option<PathBuf>)>,
    full_rebuild_every: Duration,
    kept: Arc<tokio::sync::Mutex<KeptTotals>>,
    reads: Arc<std::sync::atomic::AtomicUsize>,
}

/// What the last builds read.
#[derive(Default)]
struct KeptTotals {
    /// Turns ending before this are left out; set at each full rebuild.
    since: Option<chrono::DateTime<Utc>>,
    last_full: Option<Instant>,
    journals: HashMap<PathBuf, (FileStamp, Aggregator)>,
}

/// A journal file's size and modification time, and its `-wal`'s: a live
/// session's new turns sit in the `-wal` until SQLite checkpoints them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    db: Option<(u64, Option<std::time::SystemTime>)>,
    wal: Option<(u64, Option<std::time::SystemTime>)>,
}

impl FileStamp {
    fn of(path: &Path) -> Self {
        let stamp = |path: &Path| {
            std::fs::metadata(path)
                .ok()
                .map(|meta| (meta.len(), meta.modified().ok()))
        };
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        Self {
            db: stamp(path),
            wal: stamp(Path::new(&wal)),
        }
    }
}

impl JournalRanking {
    /// The journals in `journal_dir`, read whatever the trust.
    pub fn new(journal_dir: PathBuf) -> Self {
        Self {
            journal_dir,
            trust: None,
            full_rebuild_every: FULL_REBUILD_EVERY,
            kept: Arc::default(),
            reads: Arc::default(),
        }
    }

    /// The journals of `workspace`, read only while the user trusts it:
    /// trust is checked again at every build, so a workspace the user
    /// stops trusting stops steering choices at the next one.
    pub fn for_workspace(
        journal_dir: PathBuf,
        workspace: PathBuf,
        trust_store: Option<PathBuf>,
    ) -> Self {
        Self {
            trust: Some((workspace, trust_store)),
            ..Self::new(journal_dir)
        }
    }

    /// Read every journal again this often (rather than every hour).
    pub fn with_full_rebuild_every(mut self, every: Duration) -> Self {
        self.full_rebuild_every = every;
        self
    }

    /// How many journals the builds so far have read.
    pub fn journals_read(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

impl RankingSource for JournalRanking {
    fn build(&self) -> SnapshotFuture {
        let journal_dir = self.journal_dir.clone();
        let trust = self.trust.clone();
        let full_rebuild_every = self.full_rebuild_every;
        let kept = self.kept.clone();
        let reads = self.reads.clone();
        Box::pin(async move {
            if let Some((workspace, store)) = &trust {
                if !crate::launch::is_trusted(store.as_deref(), workspace) {
                    return Err(crate::model_report::UntrustedWorkspace {
                        workspace: workspace.clone(),
                    }
                    .into());
                }
            }
            let mut kept = kept.lock().await;
            let full = kept
                .last_full
                .is_none_or(|at| at.elapsed() >= full_rebuild_every);
            if full {
                kept.journals.clear();
                kept.since = Some(Utc::now() - chrono::Duration::days(i64::from(WINDOW_DAYS)));
                kept.last_full = Some(Instant::now());
            }
            let (journals, _) = list_session_journals(&journal_dir).await?;
            // A set: a workspace can hold thousands of journals.
            let listed: std::collections::HashSet<&PathBuf> = journals.iter().collect();
            kept.journals.retain(|path, _| listed.contains(path));
            // Stamped before reading: a journal that changes while it is
            // read is read again next time.
            let changed: Vec<(PathBuf, FileStamp)> = journals
                .into_iter()
                .map(|path| {
                    let stamp = FileStamp::of(&path);
                    (path, stamp)
                })
                .filter(|(path, stamp)| {
                    kept.journals
                        .get(path)
                        .is_none_or(|(kept_stamp, _)| kept_stamp != stamp)
                })
                .collect();
            reads.fetch_add(changed.len(), Ordering::SeqCst);
            let mut stamps: HashMap<PathBuf, FileStamp> = changed.iter().cloned().collect();
            let options = ReportOptions {
                since: kept.since,
                ..ReportOptions::default()
            };
            let paths = changed.into_iter().map(|(path, _)| path).collect();
            for (path, read) in read_journals(paths, &options).await? {
                match read {
                    Ok(totals) => {
                        let stamp = stamps.remove(&path).unwrap_or_else(|| FileStamp::of(&path));
                        kept.journals.insert(path, (stamp, totals));
                    }
                    Err(error) => {
                        tracing::debug!(path = %path.display(), %error, "journal left out of the ranking");
                        kept.journals.remove(&path);
                    }
                }
            }
            let mut totals = Aggregator::default();
            for (_, journal) in kept.journals.values() {
                totals.merge_from(journal);
            }
            Ok(RankingSnapshot::from_totals(totals))
        })
    }
}

/// Which models the account can run.
pub trait RunnableModels: Send + Sync {
    /// Whether `model` (`<provider>/<model>`) can run on `provider`; `None`
    /// when that is not known for the provider.
    fn can_run(&self, provider: &str, model: &str) -> Option<bool>;
}

/// Whether, and how often, to keep the parent's model despite a leader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exploration {
    Never,
    Always,
    /// About one in `n`, drawn from a generator seeded with `seed`.
    OneIn {
        n: u32,
        seed: u64,
    },
}

/// How an advisor behaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisorSettings {
    pub mode: SubagentModelSelection,
    /// Whether the user trusts the workspace; an untrusted one's journals
    /// are not read.
    pub trusted: bool,
    pub exploration: Exploration,
    pub stale_after: Duration,
}

/// The settings for a session of `cfg`'s workspace: its switch, the user's
/// trust (`trust_store`: `None` for the user's own; only a test passes
/// another), exploring one spawn in [`EXPLORE_ONE_IN`].
pub fn settings_for(cfg: &Config, trust_store: Option<&Path>) -> AdvisorSettings {
    AdvisorSettings {
        mode: cfg.subagents.model_selection,
        trusted: crate::launch::is_trusted(trust_store, cfg.workspace_root()),
        exploration: Exploration::OneIn {
            n: EXPLORE_ONE_IN,
            seed: uuid::Uuid::new_v4().as_u64_pair().0,
        },
        stale_after: STALE_AFTER,
    }
}

/// The latest ranking and whether one is being built. Shared by every
/// session of a project (see [`shared_cache`]).
#[derive(Default)]
pub(crate) struct RankingCache {
    snapshot: RwLock<Option<(Instant, Arc<RankingSnapshot>)>>,
    /// Why the last build failed, while no ranking is in use because of it.
    failure: RwLock<Option<String>>,
    building: AtomicBool,
}

impl RankingCache {
    fn current(&self) -> Option<(Instant, Arc<RankingSnapshot>)> {
        self.snapshot
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Whether a ranking has been built.
    #[cfg(test)]
    pub(crate) fn is_built(&self) -> bool {
        self.current().is_some()
    }

    /// When the current ranking was built.
    #[cfg(test)]
    pub(crate) fn built_at(&self) -> Option<Instant> {
        self.current().map(|(built_at, _)| built_at)
    }

    fn store(&self, snapshot: RankingSnapshot) {
        *self
            .snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some((Instant::now(), Arc::new(snapshot)));
        *self
            .failure
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    /// A build failed: no ranking is used until one succeeds (the failure
    /// can be that the workspace is no longer trusted).
    fn fail(&self, error: &anyhow::Error) {
        *self
            .snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        *self
            .failure
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(format!("{error:#}"));
    }

    fn failure(&self) -> Option<String> {
        self.failure
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// The cache for the journals in `journal_dir`, shared while any session
/// of the project holds it.
pub(crate) fn shared_cache(journal_dir: &Path) -> Arc<RankingCache> {
    static CACHES: OnceLock<Mutex<HashMap<PathBuf, Weak<RankingCache>>>> = OnceLock::new();
    let mut caches = CACHES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    caches.retain(|_, cache| cache.strong_count() > 0);
    if let Some(cache) = caches.get(journal_dir).and_then(Weak::upgrade) {
        return cache;
    }
    let cache = Arc::new(RankingCache::default());
    caches.insert(journal_dir.to_path_buf(), Arc::downgrade(&cache));
    cache
}

/// The ranking-based advisor.
pub struct RankedSubagentModels {
    source: Arc<dyn RankingSource>,
    runnable: Arc<dyn RunnableModels>,
    resolver: Arc<dyn ChildModelResolver>,
    settings: AdvisorSettings,
    cache: Arc<RankingCache>,
    /// The exploration generator's state.
    draws: Mutex<u64>,
}

impl RankedSubagentModels {
    /// An advisor with a ranking of its own, not built yet.
    pub fn new(
        source: Arc<dyn RankingSource>,
        runnable: Arc<dyn RunnableModels>,
        resolver: Arc<dyn ChildModelResolver>,
        settings: AdvisorSettings,
    ) -> Arc<Self> {
        Self::with_cache(
            source,
            runnable,
            resolver,
            settings,
            Arc::new(RankingCache::default()),
        )
    }

    pub(crate) fn with_cache(
        source: Arc<dyn RankingSource>,
        runnable: Arc<dyn RunnableModels>,
        resolver: Arc<dyn ChildModelResolver>,
        settings: AdvisorSettings,
        cache: Arc<RankingCache>,
    ) -> Arc<Self> {
        let seed = match settings.exploration {
            Exploration::OneIn { seed, .. } => seed,
            _ => 0,
        };
        Arc::new(Self {
            source,
            runnable,
            resolver,
            settings,
            cache,
            draws: Mutex::new(seed),
        })
    }

    /// Whether this advisor reads the workspace's journals at all.
    fn reads_journals(&self) -> bool {
        self.settings.mode == SubagentModelSelection::Ranked && self.settings.trusted
    }

    /// Build the ranking now, and wait for it. Does nothing when the
    /// journals are not to be read.
    pub async fn refresh(&self) -> anyhow::Result<()> {
        if !self.reads_journals() {
            return Ok(());
        }
        match self.source.build().await {
            Ok(snapshot) => {
                self.cache.store(snapshot);
                Ok(())
            }
            Err(error) => {
                self.cache.fail(&error);
                Err(error)
            }
        }
    }

    /// Build the ranking in the background if there is none yet, or it is
    /// stale: what a session does when it opens.
    pub fn warm(&self) {
        let fresh = self
            .cache
            .current()
            .is_some_and(|(built_at, _)| built_at.elapsed() < self.settings.stale_after);
        if !fresh {
            self.refresh_in_background();
        }
    }

    /// Start building the ranking, unless one is being built (or the
    /// journals are not to be read, or there is no runtime to build it on).
    /// Returns at once.
    pub fn refresh_in_background(&self) {
        if !self.reads_journals() {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.cache.building.swap(true, Ordering::SeqCst) {
            return;
        }
        let source = self.source.clone();
        let cache = self.cache.clone();
        runtime.spawn(async move {
            match source.build().await {
                Ok(snapshot) => cache.store(snapshot),
                Err(error) => {
                    tracing::warn!(%error, "could not build the model ranking");
                    cache.fail(&error);
                }
            }
            cache.building.store(false, Ordering::SeqCst);
        });
    }

    /// Whether this spawn explores: about one in `n`.
    fn explores(&self) -> bool {
        match self.settings.exploration {
            Exploration::Never => false,
            Exploration::Always => true,
            Exploration::OneIn { n, .. } => {
                let mut state = self
                    .draws
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                // splitmix64: small, fast, and good enough to spread draws.
                *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut z = *state;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                z ^= z >> 31;
                n > 0 && z.is_multiple_of(u64::from(n))
            }
        }
    }
}

fn fallback(kind: &TaskKind, reason: String, evidence: Option<ModelChoiceEvidence>) -> ModelAdvice {
    ModelAdvice {
        pick: None,
        choice: ModelChoice {
            source: ModelChoiceSource::Fallback,
            reason,
            kind: kind.clone(),
            evidence,
        },
    }
}

fn candidate(model: &RankedModel) -> RankedCandidate {
    RankedCandidate {
        model: model.model.clone(),
        provider: model.provider.clone(),
        effort: model.effort.clone(),
        turns: model.turns,
        score: model.score.unwrap_or(f64::NAN),
        rank: model.rank.unwrap_or(0),
    }
}

/// Exactly one of the effort names Forge writes (`minimal` … `max`).
fn is_known_effort(effort: &str) -> bool {
    forge_connect::effort::EffortLevel::parse(effort).is_some_and(|level| level.as_str() == effort)
}

/// `model @ effort`, the provider's default effort as `default`.
fn named(model: &str, effort: Option<&str>) -> String {
    format!("{model} @ {}", effort.unwrap_or("default"))
}

impl SubagentModelAdvisor for RankedSubagentModels {
    fn advise(&self, request: &ModelAdviceRequest<'_>) -> ModelAdvice {
        let kind = request.kind;
        let parent = request.parent;
        if self.settings.mode == SubagentModelSelection::Inherit {
            return fallback(
                kind,
                "subagent model selection is set to `inherit`: the parent's model is kept".into(),
                None,
            );
        }
        if !self.settings.trusted {
            return fallback(
                kind,
                "the workspace is not trusted, so its journals are not used to choose models"
                    .into(),
                None,
            );
        }
        let Some((built_at, snapshot)) = self.cache.current() else {
            self.refresh_in_background();
            let reason = match self.cache.failure() {
                Some(failure) => format!("the ranking of models could not be built: {failure}"),
                None => format!("the ranking of models for {kind} work is not built yet"),
            };
            return fallback(kind, reason, None);
        };
        if built_at.elapsed() >= self.settings.stale_after {
            self.refresh_in_background();
        }

        let provider = if parent.profile_id.is_empty() {
            parent
                .model
                .split_once('/')
                .map(|(prefix, _)| prefix)
                .unwrap_or_default()
        } else {
            parent.profile_id.as_str()
        };
        if provider.is_empty() {
            return fallback(kind, "the parent's provider is not known".into(), None);
        }
        if self.runnable.can_run(provider, &parent.model).is_none() {
            return fallback(
                kind,
                format!("which {provider} models the account can run is not known"),
                None,
            );
        }
        // Only the account's runnable models, and only known effort names:
        // an effort read from a journal is otherwise free text, and a
        // candidate's names end up in the reason the parent agent reads.
        let allowed = |group: &GroupRef<'_>| {
            group.provider == provider
                && self.runnable.can_run(provider, group.model) == Some(true)
                && group.effort.is_none_or(is_known_effort)
        };
        let Some(ranking) = snapshot.rank(kind, allowed, DEFAULT_MIN_TURNS) else {
            return fallback(
                kind,
                format!(
                    "no {kind} work on {provider} models the account can run in the last \
                     {WINDOW_DAYS} days"
                ),
                None,
            );
        };
        let ranked: Vec<&RankedModel> = ranking
            .models
            .iter()
            .filter(|model| model.rank.is_some())
            .collect();
        let evidence = ModelChoiceEvidence {
            window_days: WINDOW_DAYS,
            turns: ranking.turns,
            ranked_groups: ranked.len(),
            leader: ranked.first().map(|model| candidate(model)),
            runner_up: ranked.get(1).map(|model| candidate(model)),
            min_group_turns: DEFAULT_MIN_TURNS,
            min_kind_turns: MIN_KIND_TURNS,
            min_ranked_groups: MIN_RANKED_GROUPS,
        };
        if ranking.turns < MIN_KIND_TURNS {
            return fallback(
                kind,
                format!(
                    "only {} turns of {kind} work on {provider} models in the last {WINDOW_DAYS} \
                     days; {MIN_KIND_TURNS} are needed",
                    ranking.turns
                ),
                Some(evidence),
            );
        }
        if ranked.len() < MIN_RANKED_GROUPS {
            return fallback(
                kind,
                format!(
                    "only {} {provider} model(s) have {DEFAULT_MIN_TURNS} or more turns of {kind} \
                     work; {MIN_RANKED_GROUPS} are needed to compare",
                    ranked.len()
                ),
                Some(evidence),
            );
        }
        let leader = ranked[0];
        let leader_name = named(&leader.model, leader.effort.as_deref());
        if self.explores() {
            return ModelAdvice {
                pick: None,
                choice: ModelChoice {
                    source: ModelChoiceSource::Exploration,
                    reason: format!(
                        "kept the parent's model {} to keep trying it (about one spawn in \
                         {EXPLORE_ONE_IN}); the ranking's leader is {leader_name}",
                        named(&parent.model, parent.reasoning_effort.as_deref())
                    ),
                    kind: kind.clone(),
                    evidence: Some(evidence),
                },
            };
        }

        let mut passed_over = Vec::new();
        for model in &ranked {
            let request = ChildModelRequest {
                model: Some(&model.model),
                reasoning_effort: model.effort.as_deref(),
                parent,
            };
            match self.resolver.resolve(&request) {
                Ok(pick) => {
                    let name = named(&model.model, model.effort.as_deref());
                    let mut reason = format!(
                        "{name} ranks {} of {} {provider} models for {kind} work over the last \
                         {WINDOW_DAYS} days (score {:.2}, {} turns)",
                        model.rank.unwrap_or(0),
                        ranked.len(),
                        model.score.unwrap_or(f64::NAN),
                        model.turns
                    );
                    if !passed_over.is_empty() {
                        reason.push_str("; passed over: ");
                        reason.push_str(&passed_over.join("; "));
                    }
                    return ModelAdvice {
                        pick: Some(pick),
                        choice: ModelChoice {
                            source: ModelChoiceSource::Ranking,
                            reason,
                            kind: kind.clone(),
                            evidence: Some(evidence),
                        },
                    };
                }
                Err(error) => passed_over.push(format!(
                    "{} ({error})",
                    named(&model.model, model.effort.as_deref())
                )),
            }
        }
        fallback(
            kind,
            format!(
                "none of the ranked {provider} models can be used: {}",
                passed_over.join("; ")
            ),
            Some(evidence),
        )
    }
}

/// No list of the account's models: every model is unknown.
pub struct UnknownRunnableModels;

impl RunnableModels for UnknownRunnableModels {
    fn can_run(&self, _provider: &str, _model: &str) -> Option<bool> {
        None
    }
}

/// What the tier table knows of the account's models.
impl RunnableModels for crate::model_tiers::TierTable {
    fn can_run(&self, provider: &str, model: &str) -> Option<bool> {
        self.runnable_contains(provider, model)
    }
}
