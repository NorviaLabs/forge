//! The ranking of models both advisors read — the one choosing a
//! subagent's model (`subagent_models`) and Auto, choosing the main
//! session's (`auto_models`): where it comes from ([`JournalRanking`], the
//! workspace's journals, read incrementally), how it is kept (built in the
//! background, shared by a project's sessions, never waited for), and how a
//! kind's ranking is held to the evidence thresholds ([`standing`]) and
//! turned into a pick ([`first_accepted`]).

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::{Duration, Instant};

use chrono::Utc;
use forge_core::{
    ChildModel, ChildModelRequest, ChildModelResolver, ModelChoiceEvidence,
    ModelExecutionSelection, RankedCandidate,
};
use forge_types::TaskKind;

use crate::model_report::{
    read_journals, Aggregator, GroupRef, RankedModel, RankingSnapshot, ReportOptions,
    DEFAULT_MIN_TURNS,
};
use crate::workspace_journals::WorkspaceJournals;

/// How far back the turns a choice is based on go.
pub const WINDOW_DAYS: u32 = 90;
/// Turns of the kind, on candidate models, needed to choose by ranking.
pub const MIN_KIND_TURNS: usize = 20;
/// Ranked candidates needed: a ranking of one compares nothing.
pub const MIN_RANKED_GROUPS: usize = 2;
/// A ranking older than this is rebuilt (in the background) when used.
pub const STALE_AFTER: Duration = Duration::from_secs(60);

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
    /// The journals, and the trust reading them takes — checked again at
    /// every build.
    journals: WorkspaceJournals,
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
        Self::reading(WorkspaceJournals::at(journal_dir))
    }

    /// The journals `journals` may read, as they may read them.
    pub fn reading(journals: WorkspaceJournals) -> Self {
        Self {
            journals,
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
        Self::reading(WorkspaceJournals::for_workspace(
            journal_dir,
            workspace,
            trust_store,
        ))
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
        let journals = self.journals.clone();
        let full_rebuild_every = self.full_rebuild_every;
        let kept = self.kept.clone();
        let reads = self.reads.clone();
        Box::pin(async move {
            // Before anything kept is touched: an untrusted build changes
            // nothing. (The listing checks again.)
            journals.check_trust()?;
            let mut kept = kept.lock().await;
            let full = kept
                .last_full
                .is_none_or(|at| at.elapsed() >= full_rebuild_every);
            if full {
                kept.journals.clear();
                kept.since = Some(Utc::now() - chrono::Duration::days(i64::from(WINDOW_DAYS)));
                kept.last_full = Some(Instant::now());
            }
            let journals = journals.list().await?.journals;
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
    pub(crate) fn current(&self) -> Option<(Instant, Arc<RankingSnapshot>)> {
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

/// A ranking shared by a project's sessions: built in the background, never
/// waited for, rebuilt when older than `stale_after`.
pub(crate) struct SharedRanking {
    source: Arc<dyn RankingSource>,
    cache: Arc<RankingCache>,
    stale_after: Duration,
}

impl SharedRanking {
    pub(crate) fn new(
        source: Arc<dyn RankingSource>,
        cache: Arc<RankingCache>,
        stale_after: Duration,
    ) -> Self {
        Self {
            source,
            cache,
            stale_after,
        }
    }

    /// The current ranking, starting a rebuild when it is stale; or why
    /// there is none (one is then being built).
    pub(crate) fn current(&self, kind: &TaskKind) -> Result<Arc<RankingSnapshot>, String> {
        let Some((built_at, snapshot)) = self.cache.current() else {
            self.refresh_in_background();
            return Err(match self.cache.failure() {
                Some(failure) => format!("the ranking of models could not be built: {failure}"),
                None => format!("the ranking of models for {kind} work is not built yet"),
            });
        };
        if built_at.elapsed() >= self.stale_after {
            self.refresh_in_background();
        }
        Ok(snapshot)
    }

    pub(crate) async fn refresh(&self) -> anyhow::Result<()> {
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

    /// Start building, unless a build is under way (or there is no runtime
    /// to build on). Returns at once.
    pub(crate) fn refresh_in_background(&self) {
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

    /// Build in the background if there is no ranking yet, or it is stale.
    pub(crate) fn warm(&self) {
        let fresh = self
            .cache
            .current()
            .is_some_and(|(built_at, _)| built_at.elapsed() < self.stale_after);
        if !fresh {
            self.refresh_in_background();
        }
    }
}

/// The provider a selection runs on: its profile, else its model's prefix.
pub(crate) fn provider_of(selection: &ModelExecutionSelection) -> &str {
    if selection.profile_id.is_empty() {
        selection
            .model
            .split_once('/')
            .map(|(prefix, _)| prefix)
            .unwrap_or_default()
    } else {
        selection.profile_id.as_str()
    }
}

/// Where a kind stands in the ranking, held to the thresholds.
pub(crate) enum Standing {
    /// Not enough evidence to choose by: why, and what there was.
    Thin {
        reason: String,
        evidence: Option<ModelChoiceEvidence>,
    },
    /// The ranked candidates, best first.
    Ranked {
        ranked: Vec<RankedModel>,
        evidence: ModelChoiceEvidence,
    },
}

/// `kind` ranked over `provider`'s models that the account can run (with a
/// known effort name, if any), from `snapshot`, against the thresholds.
/// `reference_model` is any model of the provider, to ask whether its list
/// of runnable models is known at all.
pub(crate) fn standing(
    snapshot: &RankingSnapshot,
    kind: &TaskKind,
    provider: &str,
    reference_model: &str,
    runnable: &dyn RunnableModels,
) -> Standing {
    let thin =
        |reason: String, evidence: Option<ModelChoiceEvidence>| Standing::Thin { reason, evidence };
    if provider.is_empty() {
        return thin("the provider is not known".into(), None);
    }
    if runnable.can_run(provider, reference_model).is_none() {
        return thin(
            format!("which {provider} models the account can run is not known"),
            None,
        );
    }
    // Only the account's runnable models, and only known effort names: an
    // effort read from a journal is otherwise free text, and a candidate's
    // names end up in reasons an agent reads.
    let allowed = |group: &GroupRef<'_>| {
        group.provider == provider
            && runnable.can_run(provider, group.model) == Some(true)
            && group.effort.is_none_or(is_known_effort)
    };
    let Some(ranking) = snapshot.rank(kind, allowed, DEFAULT_MIN_TURNS) else {
        return thin(
            format!(
                "no {kind} work on {provider} models the account can run in the last \
                 {WINDOW_DAYS} days"
            ),
            None,
        );
    };
    let ranked: Vec<RankedModel> = ranking
        .models
        .into_iter()
        .filter(|model| model.rank.is_some())
        .collect();
    let evidence = ModelChoiceEvidence {
        window_days: WINDOW_DAYS,
        turns: ranking.turns,
        ranked_groups: ranked.len(),
        leader: ranked.first().map(candidate),
        runner_up: ranked.get(1).map(candidate),
        min_group_turns: DEFAULT_MIN_TURNS,
        min_kind_turns: MIN_KIND_TURNS,
        min_ranked_groups: MIN_RANKED_GROUPS,
    };
    if ranking.turns < MIN_KIND_TURNS {
        return thin(
            format!(
                "only {} turns of {kind} work on {provider} models in the last {WINDOW_DAYS} \
                 days; {MIN_KIND_TURNS} are needed",
                ranking.turns
            ),
            Some(evidence),
        );
    }
    if ranked.len() < MIN_RANKED_GROUPS {
        return thin(
            format!(
                "only {} {provider} model(s) have {DEFAULT_MIN_TURNS} or more turns of {kind} \
                 work; {MIN_RANKED_GROUPS} are needed to compare",
                ranked.len()
            ),
            Some(evidence),
        );
    }
    Standing::Ranked { ranked, evidence }
}

/// The first of `ranked` the resolver accepts, and why it was chosen; or why
/// none could be.
pub(crate) fn first_accepted(
    ranked: &[RankedModel],
    resolver: &dyn ChildModelResolver,
    parent: &ModelExecutionSelection,
    provider: &str,
    kind: &TaskKind,
) -> Result<(ChildModel, String), String> {
    let mut passed_over = Vec::new();
    for model in ranked {
        let request = ChildModelRequest {
            model: Some(&model.model),
            reasoning_effort: model.effort.as_deref(),
            parent,
        };
        match resolver.resolve(&request) {
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
                return Ok((pick, reason));
            }
            Err(error) => passed_over.push(format!(
                "{} ({error})",
                named(&model.model, model.effort.as_deref())
            )),
        }
    }
    Err(format!(
        "none of the ranked {provider} models can be used: {}",
        passed_over.join("; ")
    ))
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
pub(crate) fn named(model: &str, effort: Option<&str>) -> String {
    format!("{model} @ {}", effort.unwrap_or("default"))
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
