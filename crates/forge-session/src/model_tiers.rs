//! The model tier table: `fast` / `default` / `strong` for every provider,
//! built from models.dev ahead of time, stored once per user, and read from
//! memory when a turn asks for a tier.
//!
//! # Where the work happens
//!
//! - **Building** ([`TierTable::sync`]) runs off the request path: after a
//!   models.dev refresh, when providers change, and once while a provider is
//!   connected ([`prepare_model_tiers`], the onboarding wait). Each provider
//!   profile has a fingerprint over everything its rows depend on
//!   (`forge_connect::tiers::profile_fingerprint`); a profile whose stored
//!   fingerprint matches is skipped without being ranked, and a changed one
//!   writes only the rows that differ. A provider that disappears (a removed
//!   custom provider) is deleted.
//! - **Loading** ([`TierTable::reload`]) reads the table into memory when a
//!   session opens, only if another build changed it since (the `generation`
//!   counter).
//! - **Resolving** ([`TierTable::resolve`]) is a memory lookup: the first
//!   ranked model for the tier that the account can run (the account's
//!   model lists are also held in memory), else the current model at the
//!   tier's effort (from the table, or — for a model models.dev does not
//!   list — from Forge's own effort rules, worked out when the account's
//!   model lists are read), else no change.
//!
//! # Storage
//!
//! One SQLite file per user, `<Forge data dir>/model-tiers.db`, shared by
//! every project and session and kept across restarts:
//!
//! ```sql
//! tier_meta     (key PRIMARY KEY, value)   -- 'generation': bumped on every change
//! tier_profiles (profile_id PRIMARY KEY, fingerprint, models_dev_fetched_at, built_at)
//! tier_models   (profile_id, tier, model_id, rank, reasoning_effort, context_window,
//!                output_reserve, image_input, blended_cost, release_date,
//!                PRIMARY KEY (profile_id, tier, model_id))
//! tier_efforts  (profile_id, model_id, fast_effort, default_effort, strong_effort,
//!                PRIMARY KEY (profile_id, model_id))
//! ```

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use forge_connect::tiers::{ProfileTierBuild, TierEffortRow, TierRow};
use forge_connect::{ModelCatalogCache, ProviderSpec};
use forge_core::{ChildModel, ModelExecutionSelection, ModelTier, TierBasis, TierResolution};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};

/// The table's file name in Forge's application-data directory.
pub const TIER_TABLE_FILE: &str = "model-tiers.db";

/// The files tiers are built from and stored in. `None` is the user's own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TierPaths {
    /// The model catalog (models.dev rows and account model lists).
    pub catalog: Option<PathBuf>,
    /// The tier table.
    pub table: Option<PathBuf>,
    /// The user's provider definitions (`providers.toml`), on top of the
    /// built-in ones.
    pub providers: Option<PathBuf>,
}

impl TierPaths {
    /// The user's catalog and table.
    pub fn user_default() -> Self {
        Self::default()
    }

    pub(crate) fn from_config(cfg: &forge_config::Config) -> Self {
        Self {
            catalog: cfg.model.catalog_path.clone(),
            table: cfg.model.tiers_path.clone(),
            providers: None,
        }
    }

    fn registry(&self) -> forge_connect::ConnectRegistry {
        match &self.providers {
            Some(path) => forge_connect::loaded_registry_from(path),
            None => forge_connect::loaded_registry(),
        }
    }

    fn catalog(&self) -> ModelCatalogCache {
        match &self.catalog {
            Some(path) => ModelCatalogCache::new(path.clone()),
            None => ModelCatalogCache::user_default(),
        }
    }

    fn table_path(&self) -> Option<PathBuf> {
        self.table
            .clone()
            .or_else(|| forge_storage::forge_data_dir().map(|dir| dir.join(TIER_TABLE_FILE)))
    }
}

/// What one [`TierTable::sync`] changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Profiles whose fingerprint changed (or that are new), re-ranked.
    pub rebuilt: Vec<String>,
    /// Profiles whose fingerprint matched: not ranked, not written.
    pub unchanged: Vec<String>,
    /// Profiles no longer defined, deleted.
    pub removed: Vec<String>,
    /// `tier_models` rows inserted or updated, and deleted.
    pub models_written: usize,
    pub models_deleted: usize,
    /// `tier_efforts` rows inserted or updated, and deleted.
    pub efforts_written: usize,
    pub efforts_deleted: usize,
}

impl SyncReport {
    pub fn changed(&self) -> bool {
        !(self.rebuilt.is_empty() && self.removed.is_empty())
    }
}

#[derive(Debug, Default)]
struct ProfileTiers {
    /// Per tier, best first.
    ranked: HashMap<ModelTier, Vec<TierRow>>,
    efforts: HashMap<String, TierEffortRow>,
}

#[derive(Debug, Default)]
struct Snapshot {
    /// `None` until loaded from the table.
    generation: Option<i64>,
    profiles: HashMap<String, ProfileTiers>,
}

/// Account model lists by profile id.
type Runnable = HashMap<String, HashSet<String>>;

/// Tier efforts for the account's models the table has no row for.
type RuntimeEfforts = HashMap<String, TierEffortRow>;

/// The tier table for one file; one per process per file (see
/// [`TierTable::at`]).
pub struct TierTable {
    path: PathBuf,
    pool: tokio::sync::OnceCell<SqlitePool>,
    snapshot: RwLock<Arc<Snapshot>>,
    runnable: RwLock<Arc<Runnable>>,
    /// Read with `runnable`: efforts for runnable models models.dev does not
    /// list, from Forge's own effort rules.
    runtime_efforts: RwLock<Arc<RuntimeEfforts>>,
    runnable_at: Mutex<Option<Instant>>,
    /// One build at a time in this process; SQLite serializes processes.
    building: tokio::sync::Mutex<()>,
    /// When background maintenance last started here.
    maintained_at: Mutex<Option<Instant>>,
}

fn tier_name(tier: ModelTier) -> &'static str {
    tier.as_str()
}

fn to_i64(value: Option<usize>) -> Option<i64> {
    value.map(|value| i64::try_from(value).unwrap_or(i64::MAX))
}

fn to_usize(value: Option<i64>) -> Option<usize> {
    value.and_then(|value| usize::try_from(value).ok())
}

impl TierTable {
    /// The process's table for `path`, created on first use. Opening the
    /// database is deferred to the first read or build.
    pub fn at(path: &Path) -> Arc<TierTable> {
        static TABLES: OnceLock<Mutex<HashMap<PathBuf, Arc<TierTable>>>> = OnceLock::new();
        TABLES
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(TierTable::new(path)))
            .clone()
    }

    /// A table handle of its own; [`TierTable::at`] shares one per file.
    fn new(path: &Path) -> TierTable {
        TierTable {
            path: path.to_path_buf(),
            pool: tokio::sync::OnceCell::new(),
            snapshot: RwLock::new(Arc::default()),
            runnable: RwLock::new(Arc::default()),
            runtime_efforts: RwLock::new(Arc::default()),
            runnable_at: Mutex::new(None),
            building: tokio::sync::Mutex::new(()),
            maintained_at: Mutex::new(None),
        }
    }

    /// The table `paths` names; `None` without an application-data directory.
    pub fn for_paths(paths: &TierPaths) -> Option<Arc<TierTable>> {
        paths.table_path().map(|path| Self::at(&path))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    async fn pool(&self) -> anyhow::Result<&SqlitePool> {
        self.pool
            .get_or_try_init(|| async {
                if let Some(parent) = self.path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let options = SqliteConnectOptions::new()
                    .filename(&self.path)
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal)
                    .synchronous(SqliteSynchronous::Normal)
                    // Several Forge processes share the file.
                    .busy_timeout(Duration::from_secs(5));
                let pool = SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect_with(options)
                    .await?;
                migrate(&pool).await?;
                Ok::<_, anyhow::Error>(pool)
            })
            .await
    }

    /// Bring the stored table up to date with `profiles` and the catalog.
    /// Only profiles whose fingerprint changed are ranked and written, and
    /// of those only the rows that differ.
    pub async fn sync(
        &self,
        profiles: Vec<ProviderSpec>,
        paths: &TierPaths,
    ) -> anyhow::Result<SyncReport> {
        let _building = self.building.lock().await;
        let pool = self.pool().await?;

        // The catalog cache is not `Send`: every read of it happens inside a
        // blocking task that opens its own.
        let fingerprint_paths = paths.clone();
        let fingerprint_profiles = profiles.clone();
        let (fingerprints, fetched_at) = tokio::task::spawn_blocking(move || {
            let cache = fingerprint_paths.catalog();
            let fingerprints: Vec<(String, String)> = fingerprint_profiles
                .iter()
                .map(|profile| {
                    let rows = cache.registry_rows(&profile.id);
                    (
                        profile.id.clone(),
                        forge_connect::tiers::profile_fingerprint(profile, &rows),
                    )
                })
                .collect();
            (fingerprints, cache.registry_fetched_at())
        })
        .await?;

        let stored: HashMap<String, String> =
            sqlx::query("SELECT profile_id, fingerprint FROM tier_profiles")
                .fetch_all(pool)
                .await?
                .into_iter()
                .map(|row| (row.get("profile_id"), row.get("fingerprint")))
                .collect();

        let mut report = SyncReport::default();
        let changed: HashSet<String> = fingerprints
            .iter()
            .filter(|(id, fingerprint)| stored.get(id) != Some(fingerprint))
            .map(|(id, _)| id.clone())
            .collect();
        for (id, _) in &fingerprints {
            if !changed.contains(id) {
                report.unchanged.push(id.clone());
            }
        }
        report.unchanged.sort();
        let defined: HashSet<&str> = fingerprints.iter().map(|(id, _)| id.as_str()).collect();
        report.removed = stored
            .keys()
            .filter(|id| !defined.contains(id.as_str()))
            .cloned()
            .collect();
        report.removed.sort();
        if changed.is_empty() && report.removed.is_empty() {
            return Ok(report);
        }

        let build_paths = paths.clone();
        let to_build: Vec<ProviderSpec> = profiles
            .into_iter()
            .filter(|profile| changed.contains(&profile.id))
            .collect();
        let builds: Vec<ProfileTierBuild> = tokio::task::spawn_blocking(move || {
            let cache = build_paths.catalog();
            to_build
                .iter()
                .map(|profile| forge_connect::tiers::build_profile_tiers(profile, &cache))
                .collect()
        })
        .await?;

        let fetched_at = fetched_at
            .and_then(|secs| DateTime::<Utc>::from_timestamp(secs as i64, 0))
            .map(|at| at.to_rfc3339());
        let now = Utc::now().to_rfc3339();
        let mut tx = pool.begin().await?;
        for build in &builds {
            write_profile(&mut tx, build, fetched_at.as_deref(), &now, &mut report).await?;
            report.rebuilt.push(build.profile_id.clone());
        }
        for id in &report.removed {
            report.models_deleted += sqlx::query("DELETE FROM tier_models WHERE profile_id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?
                .rows_affected() as usize;
            report.efforts_deleted += sqlx::query("DELETE FROM tier_efforts WHERE profile_id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?
                .rows_affected() as usize;
            sqlx::query("DELETE FROM tier_profiles WHERE profile_id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query(
            "INSERT INTO tier_meta (key, value) VALUES ('generation', 1) \
             ON CONFLICT(key) DO UPDATE SET value = value + 1",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        report.rebuilt.sort();
        Ok(report)
    }

    /// Load the stored table into memory if it changed since the last load.
    pub async fn reload(&self) -> anyhow::Result<()> {
        let pool = self.pool().await?;
        let generation: i64 = sqlx::query(
            "SELECT COALESCE(MAX(value), 0) AS g FROM tier_meta WHERE key = 'generation'",
        )
        .fetch_one(pool)
        .await?
        .get("g");
        if self.snapshot_read().generation == Some(generation) {
            return Ok(());
        }
        let mut profiles: HashMap<String, ProfileTiers> = HashMap::new();
        for row in sqlx::query("SELECT * FROM tier_models ORDER BY profile_id, tier, rank")
            .fetch_all(pool)
            .await?
        {
            let Some(tier) = ModelTier::parse(row.get::<String, _>("tier").as_str()) else {
                continue;
            };
            let profile: String = row.get("profile_id");
            profiles
                .entry(profile)
                .or_default()
                .ranked
                .entry(tier)
                .or_default()
                .push(read_model_row(&row, tier));
        }
        for row in sqlx::query("SELECT * FROM tier_efforts")
            .fetch_all(pool)
            .await?
        {
            let effort = read_effort_row(&row);
            profiles
                .entry(row.get("profile_id"))
                .or_default()
                .efforts
                .insert(effort.model_id.clone(), effort);
        }
        *self
            .snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(Snapshot {
            generation: Some(generation),
            profiles,
        });
        Ok(())
    }

    fn snapshot_read(&self) -> Arc<Snapshot> {
        self.snapshot
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Whether the table has been loaded into memory at least once.
    pub fn loaded(&self) -> bool {
        self.snapshot_read().generation.is_some()
    }

    /// Whether any provider has ranked models in memory.
    pub fn has_rankings(&self) -> bool {
        self.snapshot_read()
            .profiles
            .values()
            .any(|profile| !profile.ranked.is_empty())
    }

    /// Replace the efforts used for models the table has no row for.
    pub fn set_runtime_efforts(&self, efforts: HashMap<String, TierEffortRow>) {
        *self
            .runtime_efforts
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(efforts);
    }

    /// Replace the account model lists resolution filters by.
    pub fn set_runnable(&self, runnable: HashMap<String, HashSet<String>>) {
        *self
            .runnable
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(runnable);
        *self
            .runnable_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
    }

    /// Re-read which models each connected account can run (from the
    /// cached account catalogs; no network), unless that happened within
    /// `max_age`.
    pub async fn refresh_runnable(
        &self,
        paths: &TierPaths,
        max_age: Duration,
    ) -> anyhow::Result<()> {
        let fresh = self
            .runnable_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some_and(|at| at.elapsed() < max_age);
        if fresh {
            return Ok(());
        }
        let paths = paths.clone();
        let (runnable, efforts) = tokio::task::spawn_blocking(move || {
            let registry = paths.registry();
            let store = forge_connect::CredentialStore::user_default();
            let cache = paths.catalog();
            let mut runnable: Runnable = HashMap::new();
            let mut efforts: RuntimeEfforts = HashMap::new();
            for entry in forge_connect::runnable_models_for_picker(
                registry.profiles(),
                &store,
                &cache,
                false,
            ) {
                if let Some(row) = registry.get(&entry.profile_id).and_then(|profile| {
                    forge_connect::tiers::model_tier_efforts(profile, &entry.id, &cache)
                }) {
                    efforts.insert(entry.id.clone(), row);
                }
                runnable
                    .entry(entry.profile_id)
                    .or_default()
                    .insert(entry.id);
            }
            (runnable, efforts)
        })
        .await?;
        self.set_runtime_efforts(efforts);
        self.set_runnable(runnable);
        Ok(())
    }

    /// Resolve `tier` for a session on `current`, from memory only.
    pub fn resolve(&self, tier: ModelTier, current: &ModelExecutionSelection) -> TierResolution {
        let profile_id = if current.profile_id.is_empty() {
            current
                .model
                .split_once('/')
                .map(|(prefix, _)| prefix.to_string())
                .unwrap_or_default()
        } else {
            current.profile_id.clone()
        };
        let snapshot = self.snapshot_read();
        let profile = snapshot.profiles.get(&profile_id);
        let runnable = self
            .runnable
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let chosen = runnable.get(&profile_id).and_then(|runnable| {
            profile?
                .ranked
                .get(&tier)?
                .iter()
                .find(|row| runnable.contains(&row.model_id))
        });
        if let Some(row) = chosen {
            return TierResolution {
                tier,
                model: ChildModel {
                    model: row.model_id.clone(),
                    profile_id,
                    reasoning_effort: row.reasoning_effort.clone(),
                    context_window: row.context_window,
                    output_reserve: row.output_reserve,
                    image_input: Some(row.image_input),
                },
                basis: TierBasis::Catalog,
            };
        }
        let runtime = self
            .runtime_efforts
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        // The table's efforts, else Forge's rules for a model models.dev
        // does not list.
        let effort = profile
            .and_then(|profile| profile.efforts.get(&current.model))
            .or_else(|| runtime.get(&current.model))
            .and_then(|efforts| efforts.get(tier));
        match effort {
            Some(effort) => TierResolution {
                tier,
                model: ChildModel {
                    model: current.model.clone(),
                    profile_id,
                    reasoning_effort: Some(effort.to_string()),
                    context_window: None,
                    output_reserve: None,
                    image_input: None,
                },
                basis: TierBasis::EffortOnly,
            },
            None => TierResolution::unavailable(tier, current),
        }
    }

    /// Start [`prepare_model_tiers`] in the background, at most once per
    /// `min_interval` in this process. Never waits.
    pub(crate) fn maintain_in_background(
        self: &Arc<Self>,
        paths: TierPaths,
        min_interval: Duration,
    ) {
        {
            let mut maintained = self
                .maintained_at
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if maintained.is_some_and(|at| at.elapsed() < min_interval) {
                return;
            }
            *maintained = Some(Instant::now());
        }
        tokio::spawn(async move {
            if let Err(error) = prepare_model_tiers(paths, None).await {
                tracing::debug!(%error, "background model tier maintenance failed");
            }
        });
    }
}

async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
    for statement in [
        "CREATE TABLE IF NOT EXISTS tier_meta (
            key   TEXT PRIMARY KEY,
            value INTEGER NOT NULL
        )",
        "CREATE TABLE IF NOT EXISTS tier_profiles (
            profile_id            TEXT PRIMARY KEY,
            fingerprint           TEXT NOT NULL,
            models_dev_fetched_at TEXT,
            built_at              TEXT NOT NULL
        )",
        "CREATE TABLE IF NOT EXISTS tier_models (
            profile_id       TEXT    NOT NULL,
            tier             TEXT    NOT NULL CHECK (tier IN ('fast', 'default', 'strong')),
            model_id         TEXT    NOT NULL,
            rank             INTEGER NOT NULL,
            reasoning_effort TEXT,
            context_window   INTEGER,
            output_reserve   INTEGER,
            image_input      INTEGER NOT NULL,
            blended_cost     REAL    NOT NULL,
            release_date     TEXT,
            PRIMARY KEY (profile_id, tier, model_id)
        )",
        "CREATE TABLE IF NOT EXISTS tier_efforts (
            profile_id     TEXT NOT NULL,
            model_id       TEXT NOT NULL,
            fast_effort    TEXT,
            default_effort TEXT,
            strong_effort  TEXT,
            PRIMARY KEY (profile_id, model_id)
        )",
    ] {
        sqlx::query(statement).execute(pool).await?;
    }
    Ok(())
}

fn read_model_row(row: &sqlx::sqlite::SqliteRow, tier: ModelTier) -> TierRow {
    TierRow {
        tier,
        rank: u32::try_from(row.get::<i64, _>("rank")).unwrap_or(u32::MAX),
        model_id: row.get("model_id"),
        reasoning_effort: row.get("reasoning_effort"),
        context_window: to_usize(row.get("context_window")),
        output_reserve: to_usize(row.get("output_reserve")),
        image_input: row.get("image_input"),
        blended_cost: row.get("blended_cost"),
        release_date: row.get("release_date"),
    }
}

fn read_effort_row(row: &sqlx::sqlite::SqliteRow) -> TierEffortRow {
    TierEffortRow {
        model_id: row.get("model_id"),
        fast: row.get("fast_effort"),
        default: row.get("default_effort"),
        strong: row.get("strong_effort"),
    }
}

/// Write one rebuilt profile: only rows that differ from what is stored.
async fn write_profile(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    build: &ProfileTierBuild,
    fetched_at: Option<&str>,
    now: &str,
    report: &mut SyncReport,
) -> anyhow::Result<()> {
    let id = build.profile_id.as_str();

    let mut stored_models: HashMap<(ModelTier, String), TierRow> = HashMap::new();
    for row in sqlx::query("SELECT * FROM tier_models WHERE profile_id = ?")
        .bind(id)
        .fetch_all(&mut **tx)
        .await?
    {
        if let Some(tier) = ModelTier::parse(row.get::<String, _>("tier").as_str()) {
            let model = read_model_row(&row, tier);
            stored_models.insert((tier, model.model_id.clone()), model);
        }
    }
    for model in &build.rows {
        let key = (model.tier, model.model_id.clone());
        if stored_models.remove(&key).as_ref() == Some(model) {
            continue;
        }
        sqlx::query(
            "INSERT OR REPLACE INTO tier_models (profile_id, tier, model_id, rank, \
             reasoning_effort, context_window, output_reserve, image_input, blended_cost, \
             release_date) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(tier_name(model.tier))
        .bind(&model.model_id)
        .bind(i64::from(model.rank))
        .bind(model.reasoning_effort.as_deref())
        .bind(to_i64(model.context_window))
        .bind(to_i64(model.output_reserve))
        .bind(model.image_input)
        .bind(model.blended_cost)
        .bind(model.release_date.as_deref())
        .execute(&mut **tx)
        .await?;
        report.models_written += 1;
    }
    for (tier, model_id) in stored_models.into_keys() {
        sqlx::query("DELETE FROM tier_models WHERE profile_id = ? AND tier = ? AND model_id = ?")
            .bind(id)
            .bind(tier_name(tier))
            .bind(model_id)
            .execute(&mut **tx)
            .await?;
        report.models_deleted += 1;
    }

    let mut stored_efforts: HashMap<String, TierEffortRow> = HashMap::new();
    for row in sqlx::query("SELECT * FROM tier_efforts WHERE profile_id = ?")
        .bind(id)
        .fetch_all(&mut **tx)
        .await?
    {
        let effort = read_effort_row(&row);
        stored_efforts.insert(effort.model_id.clone(), effort);
    }
    for effort in &build.efforts {
        if stored_efforts.remove(&effort.model_id).as_ref() == Some(effort) {
            continue;
        }
        sqlx::query(
            "INSERT OR REPLACE INTO tier_efforts (profile_id, model_id, fast_effort, \
             default_effort, strong_effort) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(&effort.model_id)
        .bind(effort.fast.as_deref())
        .bind(effort.default.as_deref())
        .bind(effort.strong.as_deref())
        .execute(&mut **tx)
        .await?;
        report.efforts_written += 1;
    }
    for model_id in stored_efforts.into_keys() {
        sqlx::query("DELETE FROM tier_efforts WHERE profile_id = ? AND model_id = ?")
            .bind(id)
            .bind(model_id)
            .execute(&mut **tx)
            .await?;
        report.efforts_deleted += 1;
    }

    sqlx::query(
        "INSERT OR REPLACE INTO tier_profiles (profile_id, fingerprint, models_dev_fetched_at, \
         built_at) VALUES (?, ?, ?, ?)",
    )
    .bind(id)
    .bind(&build.fingerprint)
    .bind(fetched_at)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Bring the tier table up to date and load it, downloading models.dev
/// first when the local copy is stale or predates tier metadata. With
/// `connected`, that provider's account model list is refreshed too, so its
/// tiers can be used at once. This is the onboarding wait when a provider
/// is connected; everywhere else it runs in the background. The download is
/// bounded by the registry's own timeout; a failed download leaves the table
/// as it was and tiers keep working from what is stored (or change nothing).
pub async fn prepare_model_tiers(
    paths: TierPaths,
    connected: Option<String>,
) -> anyhow::Result<SyncReport> {
    let Some(table) = TierTable::for_paths(&paths) else {
        anyhow::bail!("no application-data directory for the model tier table");
    };
    let fetch_paths = paths.clone();
    let profiles = tokio::task::spawn_blocking(move || {
        let registry = fetch_paths.registry();
        let cache = fetch_paths.catalog();
        forge_connect::refresh_models_dev_registry_if_stale(registry.profiles(), &cache);
        if let Some(profile) = connected.and_then(|id| registry.get(&id).cloned()) {
            let store = forge_connect::CredentialStore::user_default();
            let _ = forge_connect::refresh_profile_catalog(&profile, &store, &cache);
        }
        registry.profiles().to_vec()
    })
    .await?;
    let report = table.sync(profiles, &paths).await?;
    table.reload().await?;
    table.refresh_runnable(&paths, Duration::ZERO).await?;
    Ok(report)
}

/// Resolves tiers from a [`TierTable`] in memory.
pub(crate) struct TableTiers(pub(crate) Arc<TierTable>);

impl forge_core::ModelTierResolver for TableTiers {
    fn resolve_tier(&self, request: &forge_core::TierRequest<'_>) -> TierResolution {
        self.0.resolve(request.tier, request.current)
    }
}

#[cfg(test)]
mod tests;
