//! Repository-wide task roster, lifecycle state and exclusive ownership.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use forge_storage::WorktreeRecord;
use forge_types::SessionId;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeOwnership {
    Primary,
    Managed,
    Attached,
}

impl WorktreeOwnership {
    fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Managed => "managed",
            Self::Attached => "attached",
        }
    }

    fn parse(value: &str) -> Result<Self, RepositoryTaskError> {
        match value {
            "primary" => Ok(Self::Primary),
            "managed" => Ok(Self::Managed),
            "attached" => Ok(Self::Attached),
            value => Err(RepositoryTaskError::InvalidStoredValue {
                field: "ownership",
                value: value.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLifecycle {
    Active,
    Archived,
    Unavailable,
}

impl SessionLifecycle {
    fn parse(value: &str) -> Result<Self, RepositoryTaskError> {
        match value {
            "active" => Ok(Self::Active),
            "archived" => Ok(Self::Archived),
            "unavailable" => Ok(Self::Unavailable),
            value => Err(RepositoryTaskError::InvalidStoredValue {
                field: "lifecycle",
                value: value.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorTurnState {
    Idle,
    Queued,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl SupervisorTurnState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    fn as_str(self) -> &'static str {
        self.label()
    }

    fn parse(value: &str) -> Result<Self, RepositoryTaskError> {
        match value {
            "idle" => Ok(Self::Idle),
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "waiting" => Ok(Self::Waiting),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "interrupted" => Ok(Self::Interrupted),
            value => Err(RepositoryTaskError::InvalidStoredValue {
                field: "turn_state",
                value: value.into(),
            }),
        }
    }

    fn archive_allowed(self) -> bool {
        !matches!(self, Self::Queued | Self::Running | Self::Waiting)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryTask {
    pub session_id: SessionId,
    pub label: String,
    pub workspace: PathBuf,
    pub branch: String,
    pub ownership: WorktreeOwnership,
    pub lifecycle: SessionLifecycle,
    pub turn_state: SupervisorTurnState,
    pub slot: Option<u8>,
    pub model_id: String,
    pub route_id: String,
    pub reasoning_effort: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewRepositoryTask {
    pub session_id: SessionId,
    pub label: String,
    pub workspace: PathBuf,
    pub branch: String,
    pub ownership: WorktreeOwnership,
    pub slot: Option<u8>,
    pub model_id: String,
    pub route_id: String,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCreation {
    pub operation_id: u64,
    pub label: String,
    pub source_workspace: PathBuf,
    /// Prompt held back until trust is granted; see
    /// [`RepositoryControl::begin_managed_creation`].
    pub first_prompt: Option<String>,
}

/// What a finished managed creation hands back to the supervisor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompletedCreation {
    pub session_id: Option<SessionId>,
    pub first_prompt: Option<String>,
}

/// A managed creation left unresolved by an earlier process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleCreation {
    pub operation_id: u64,
    pub label: String,
    pub workspace: Option<PathBuf>,
    pub session_id: Option<SessionId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaseOwner {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub workspace: PathBuf,
}

#[derive(Debug)]
pub struct RepositoryLease {
    file: File,
    owner: LeaseOwner,
}

impl RepositoryLease {
    pub fn acquire(control_dir: &Path, workspace: &Path) -> Result<Self, RepositoryTaskError> {
        std::fs::create_dir_all(control_dir)?;
        let path = control_dir.join("owner.lock");
        let mut file = OpenOptions::new()
            .create(true)
            // Never truncate on open: the incumbent owner's metadata has to
            // survive so a losing process can read and report it.
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        if let Err(error) = file.try_lock() {
            let mut text = String::new();
            file.read_to_string(&mut text)?;
            let owner = serde_json::from_str(&text).ok();
            return Err(RepositoryTaskError::AlreadyOwned {
                path,
                owner,
                source: error.into(),
            });
        }
        let owner = LeaseOwner {
            pid: std::process::id(),
            started_at: Utc::now(),
            workspace: workspace.to_path_buf(),
        };
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(serde_json::to_string(&owner)?.as_bytes())?;
        file.sync_data()?;
        Ok(Self { file, owner })
    }

    pub fn owner(&self) -> &LeaseOwner {
        &self.owner
    }
}

impl Drop for RepositoryLease {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RepositoryTaskError {
    #[error("repository task database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("repository task storage error: {0}")]
    Io(#[from] std::io::Error),
    #[error("repository task serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("another Forge process already owns this repository group ({}). \
             Close it, or run Forge from a different repository.", describe_owner(.owner))]
    AlreadyOwned {
        path: PathBuf,
        owner: Option<LeaseOwner>,
        source: std::io::Error,
    },
    #[error("worktree {} is already bound to a live task", .workspace.display())]
    WorkspaceInUse {
        workspace: PathBuf,
        session_id: SessionId,
    },
    #[error("stored {field} has unsupported value `{value}`")]
    InvalidStoredValue { field: &'static str, value: String },
    #[error("stored session id is invalid: {0}")]
    InvalidSessionId(String),
    #[error("task `{0}` was not found")]
    NotFound(SessionId),
    #[error("task `{0}` must be stopped before it can be archived")]
    MustStopBeforeArchive(SessionId),
    #[error("task `{0}` must be trusted before it can run")]
    TrustRequired(SessionId),
    #[error("task `{0}` is archived")]
    Archived(SessionId),
    #[error("slot {slot} is already assigned to task `{session_id}`")]
    SlotOccupied { slot: u8, session_id: SessionId },
}

pub struct RepositoryControl {
    pool: SqlitePool,
    path: PathBuf,
}

impl RepositoryControl {
    pub async fn open(control_dir: &Path) -> Result<Self, RepositoryTaskError> {
        std::fs::create_dir_all(control_dir)?;
        let path = control_dir.join("tasks.db");
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        let control = Self { pool, path };
        control.migrate().await?;
        Ok(control)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    async fn migrate(&self) -> Result<(), RepositoryTaskError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS repository_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                selected_session_id TEXT
            );
            INSERT OR IGNORE INTO repository_state (id) VALUES (1);

            CREATE TABLE IF NOT EXISTS tasks (
                session_id TEXT PRIMARY KEY,
                label TEXT NOT NULL,
                workspace TEXT NOT NULL,
                branch TEXT NOT NULL,
                ownership TEXT NOT NULL,
                lifecycle TEXT NOT NULL,
                turn_state TEXT NOT NULL,
                slot INTEGER,
                model_id TEXT NOT NULL,
                route_id TEXT NOT NULL,
                reasoning_effort TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                archived_at TEXT
            );
            CREATE UNIQUE INDEX IF NOT EXISTS tasks_live_workspace
                ON tasks(workspace) WHERE lifecycle <> 'archived';
            CREATE UNIQUE INDEX IF NOT EXISTS tasks_live_slot
                ON tasks(slot) WHERE lifecycle <> 'archived' AND slot IS NOT NULL;

            CREATE TABLE IF NOT EXISTS pending_operations (
                operation_id INTEGER PRIMARY KEY AUTOINCREMENT,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                label TEXT NOT NULL,
                source_workspace TEXT NOT NULL,
                target_workspace TEXT,
                branch TEXT,
                session_id TEXT,
                error TEXT,
                first_prompt TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS prompt_queue (
                queue_id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                text TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS prompt_queue_dispatch
                ON prompt_queue(status, queue_id);
            "#,
        )
        .execute(&self.pool)
        .await?;
        self.add_missing_columns().await?;
        Ok(())
    }

    /// Columns added after the first schema shipped. SQLite has no
    /// `ADD COLUMN IF NOT EXISTS`, so read `table_info` and add only what a
    /// pre-existing control database is actually missing.
    async fn add_missing_columns(&self) -> Result<(), RepositoryTaskError> {
        let existing: Vec<String> = sqlx::query("PRAGMA table_info(pending_operations)")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(|row| row.get::<String, _>("name"))
            .collect();
        if !existing.iter().any(|name| name == "first_prompt") {
            sqlx::query("ALTER TABLE pending_operations ADD COLUMN first_prompt TEXT")
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// Mark a managed creation complete once trust has been granted, handing
    /// back the queued first prompt so the caller can enqueue it *after* the
    /// trust gate rather than before it.
    pub async fn complete_creation(
        &self,
        operation_id: u64,
    ) -> Result<CompletedCreation, RepositoryTaskError> {
        let row = sqlx::query(
            "SELECT session_id, first_prompt FROM pending_operations WHERE operation_id = ? \
             AND status IN ('worktree_created', 'awaiting_trust')",
        )
        .bind(operation_id as i64)
        .fetch_optional(&self.pool)
        .await?;
        let session_id = row
            .as_ref()
            .and_then(|row| row.get::<Option<String>, _>("session_id"))
            .map(parse_session_id)
            .transpose()?;
        let first_prompt = row.and_then(|row| row.get::<Option<String>, _>("first_prompt"));
        let result = sqlx::query(
            "UPDATE pending_operations SET status = 'completed', updated_at = ? \
             WHERE operation_id = ? AND status IN ('worktree_created', 'awaiting_trust')",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(operation_id as i64)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(RepositoryTaskError::InvalidStoredValue {
                field: "pending_operation",
                value: operation_id.to_string(),
            });
        }
        Ok(CompletedCreation {
            session_id,
            first_prompt,
        })
    }

    pub async fn cancel_creation(
        &self,
        operation_id: u64,
        error: &str,
    ) -> Result<(Option<PathBuf>, Option<SessionId>), RepositoryTaskError> {
        let row = sqlx::query(
            "SELECT target_workspace, session_id, status FROM pending_operations \
             WHERE operation_id = ?",
        )
        .bind(operation_id as i64)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(RepositoryTaskError::InvalidStoredValue {
            field: "pending_operation",
            value: operation_id.to_string(),
        })?;
        let status = row.get::<String, _>("status");
        let target = row
            .get::<Option<String>, _>("target_workspace")
            .map(PathBuf::from);
        let session_id = row
            .get::<Option<String>, _>("session_id")
            .map(parse_session_id)
            .transpose()?;
        if status == "completed" {
            return Ok((target, session_id));
        }
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "UPDATE pending_operations SET status = 'cancelled', error = ?, updated_at = ? \
             WHERE operation_id = ?",
        )
        .bind(error)
        .bind(Utc::now().to_rfc3339())
        .bind(operation_id as i64)
        .execute(&mut *transaction)
        .await?;
        if let Some(session_id) = session_id {
            sqlx::query("DELETE FROM tasks WHERE session_id = ?")
                .bind(session_id.to_string())
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        Ok((target, session_id))
    }

    pub async fn enqueue_prompt(
        &self,
        session_id: SessionId,
        text: &str,
    ) -> Result<u64, RepositoryTaskError> {
        let task = self.task(session_id).await?;
        if task.lifecycle == SessionLifecycle::Archived {
            return Err(RepositoryTaskError::Archived(session_id));
        }
        let awaiting_trust = sqlx::query(
            "SELECT 1 FROM pending_operations WHERE session_id = ? \
             AND status = 'awaiting_trust' LIMIT 1",
        )
        .bind(session_id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .is_some();
        if awaiting_trust {
            return Err(RepositoryTaskError::TrustRequired(session_id));
        }
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "INSERT INTO prompt_queue (session_id, text, status, created_at, updated_at) \
             VALUES (?, ?, 'queued', ?, ?)",
        )
        .bind(session_id.to_string())
        .bind(text)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid() as u64)
    }

    pub async fn queued_prompts(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<(u64, String)>, RepositoryTaskError> {
        let rows = sqlx::query(
            "SELECT queue_id, text FROM prompt_queue WHERE session_id = ? \
             AND status = 'queued' ORDER BY queue_id",
        )
        .bind(session_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                (
                    row.get::<i64, _>("queue_id") as u64,
                    row.get::<String, _>("text"),
                )
            })
            .collect())
    }

    pub async fn claim_next_prompt(
        &self,
        session_id: SessionId,
    ) -> Result<Option<(u64, String)>, RepositoryTaskError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT queue_id, text FROM prompt_queue WHERE session_id = ? \
             AND status = 'queued' ORDER BY queue_id LIMIT 1",
        )
        .bind(session_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.commit().await?;
            return Ok(None);
        };
        let queue_id = row.get::<i64, _>("queue_id") as u64;
        let text = row.get::<String, _>("text");
        sqlx::query(
            "UPDATE prompt_queue SET status = 'running', updated_at = ? WHERE queue_id = ?",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(queue_id as i64)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(Some((queue_id, text)))
    }

    pub async fn finish_prompt(
        &self,
        queue_id: u64,
        status: &str,
    ) -> Result<(), RepositoryTaskError> {
        sqlx::query("UPDATE prompt_queue SET status = ?, updated_at = ? WHERE queue_id = ?")
            .bind(status)
            .bind(Utc::now().to_rfc3339())
            .bind(queue_id as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Open a managed-worktree creation. `first_prompt` is *parked* on the
    /// operation rather than queued: a task in `awaiting_trust` must reject
    /// prompts, so the prompt is handed back by [`Self::complete_creation`]
    /// once the operator has actually granted trust.
    pub async fn begin_managed_creation(
        &self,
        label: &str,
        source_workspace: &Path,
        first_prompt: Option<&str>,
    ) -> Result<PendingCreation, RepositoryTaskError> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "INSERT INTO pending_operations \
             (kind, status, label, source_workspace, first_prompt, created_at, updated_at) \
             VALUES ('create_managed', 'pending', ?, ?, ?, ?, ?)",
        )
        .bind(label)
        .bind(source_workspace.display().to_string())
        .bind(first_prompt)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(PendingCreation {
            operation_id: result.last_insert_rowid() as u64,
            label: label.to_string(),
            source_workspace: source_workspace.to_path_buf(),
            first_prompt: first_prompt.map(str::to_string),
        })
    }

    /// Managed creations that were interrupted before trust was resolved.
    /// A restart must surface these rather than leave an orphan worktree and
    /// a task row that can never accept a prompt.
    pub async fn stale_creations(&self) -> Result<Vec<StaleCreation>, RepositoryTaskError> {
        let rows = sqlx::query(
            "SELECT operation_id, label, target_workspace, session_id FROM pending_operations \
             WHERE kind = 'create_managed' AND status IN ('pending', 'worktree_created', \
             'awaiting_trust') ORDER BY operation_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(StaleCreation {
                    operation_id: row.get::<i64, _>("operation_id") as u64,
                    label: row.get("label"),
                    workspace: row
                        .get::<Option<String>, _>("target_workspace")
                        .map(PathBuf::from),
                    session_id: row
                        .get::<Option<String>, _>("session_id")
                        .map(parse_session_id)
                        .transpose()?,
                })
            })
            .collect()
    }

    pub async fn mark_worktree_created(
        &self,
        operation_id: u64,
        target_workspace: &Path,
        branch: &str,
    ) -> Result<(), RepositoryTaskError> {
        sqlx::query(
            "UPDATE pending_operations SET status = 'worktree_created', \
             target_workspace = ?, branch = ?, updated_at = ? WHERE operation_id = ?",
        )
        .bind(target_workspace.display().to_string())
        .bind(branch)
        .bind(Utc::now().to_rfc3339())
        .bind(operation_id as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn register_task(
        &self,
        task: NewRepositoryTask,
        operation_id: Option<u64>,
    ) -> Result<(), RepositoryTaskError> {
        let now = Utc::now().to_rfc3339();
        let mut transaction = self.pool.begin().await?;
        // The unique index below would also catch this, but as an opaque
        // constraint violation. Attaching a worktree twice is an ordinary
        // operator mistake and deserves a message that names the conflict.
        if let Some(row) = sqlx::query(
            "SELECT session_id FROM tasks WHERE workspace = ? AND lifecycle <> 'archived'",
        )
        .bind(task.workspace.display().to_string())
        .fetch_optional(&mut *transaction)
        .await?
        {
            return Err(RepositoryTaskError::WorkspaceInUse {
                workspace: task.workspace,
                session_id: parse_session_id(row.get::<String, _>("session_id"))?,
            });
        }
        sqlx::query(
            "INSERT INTO tasks \
             (session_id, label, workspace, branch, ownership, lifecycle, turn_state, slot, \
              model_id, route_id, reasoning_effort, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, 'active', 'idle', ?, ?, ?, ?, ?, ?)",
        )
        .bind(task.session_id.to_string())
        .bind(task.label)
        .bind(task.workspace.display().to_string())
        .bind(task.branch)
        .bind(task.ownership.as_str())
        .bind(task.slot.map(i64::from))
        .bind(task.model_id)
        .bind(task.route_id)
        .bind(task.reasoning_effort)
        .bind(&now)
        .bind(&now)
        .execute(&mut *transaction)
        .await?;
        if let Some(operation_id) = operation_id {
            sqlx::query(
                "UPDATE pending_operations SET status = 'awaiting_trust', session_id = ?, \
                 updated_at = ? WHERE operation_id = ?",
            )
            .bind(task.session_id.to_string())
            .bind(&now)
            .bind(operation_id as i64)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn tasks(&self) -> Result<Vec<RepositoryTask>, RepositoryTaskError> {
        let rows = sqlx::query(
            "SELECT * FROM tasks ORDER BY \
             CASE lifecycle WHEN 'active' THEN 0 WHEN 'unavailable' THEN 1 ELSE 2 END, \
             CASE WHEN slot IS NULL THEN 10 ELSE slot END, updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(parse_task).collect()
    }

    pub async fn task(&self, session_id: SessionId) -> Result<RepositoryTask, RepositoryTaskError> {
        let row = sqlx::query("SELECT * FROM tasks WHERE session_id = ?")
            .bind(session_id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(RepositoryTaskError::NotFound(session_id))?;
        parse_task(&row)
    }

    pub async fn set_turn_state(
        &self,
        session_id: SessionId,
        state: SupervisorTurnState,
    ) -> Result<(), RepositoryTaskError> {
        let result = sqlx::query(
            "UPDATE tasks SET turn_state = ?, updated_at = ? WHERE session_id = ? \
             AND lifecycle <> 'archived'",
        )
        .bind(state.as_str())
        .bind(Utc::now().to_rfc3339())
        .bind(session_id.to_string())
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(RepositoryTaskError::NotFound(session_id));
        }
        Ok(())
    }

    pub async fn archive(&self, session_id: SessionId) -> Result<(), RepositoryTaskError> {
        let task = self.task(session_id).await?;
        if !task.turn_state.archive_allowed() {
            return Err(RepositoryTaskError::MustStopBeforeArchive(session_id));
        }
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE tasks SET lifecycle = 'archived', slot = NULL, archived_at = ?, \
             updated_at = ? WHERE session_id = ?",
        )
        .bind(&now)
        .bind(&now)
        .bind(session_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_selected(
        &self,
        session_id: Option<SessionId>,
    ) -> Result<(), RepositoryTaskError> {
        sqlx::query("UPDATE repository_state SET selected_session_id = ? WHERE id = 1")
            .bind(session_id.map(|id| id.to_string()))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn selected(&self) -> Result<Option<SessionId>, RepositoryTaskError> {
        let value = sqlx::query("SELECT selected_session_id FROM repository_state WHERE id = 1")
            .fetch_one(&self.pool)
            .await?
            .get::<Option<String>, _>("selected_session_id");
        value.map(parse_session_id).transpose()
    }

    pub async fn assign_slot(
        &self,
        session_id: SessionId,
        slot: Option<u8>,
        swap: bool,
    ) -> Result<(), RepositoryTaskError> {
        let mut transaction = self.pool.begin().await?;
        if let Some(slot) = slot {
            let occupant = sqlx::query(
                "SELECT session_id FROM tasks WHERE slot = ? AND lifecycle <> 'archived' \
                 AND session_id <> ?",
            )
            .bind(i64::from(slot))
            .bind(session_id.to_string())
            .fetch_optional(&mut *transaction)
            .await?;
            if let Some(row) = occupant {
                let occupant = parse_session_id(row.get::<String, _>("session_id"))?;
                if !swap {
                    return Err(RepositoryTaskError::SlotOccupied {
                        slot,
                        session_id: occupant,
                    });
                }
                sqlx::query("UPDATE tasks SET slot = NULL WHERE session_id = ?")
                    .bind(occupant.to_string())
                    .execute(&mut *transaction)
                    .await?;
            }
        }
        let result = sqlx::query(
            "UPDATE tasks SET slot = ?, updated_at = ? WHERE session_id = ? \
             AND lifecycle <> 'archived'",
        )
        .bind(slot.map(i64::from))
        .bind(Utc::now().to_rfc3339())
        .bind(session_id.to_string())
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() == 0 {
            return Err(RepositoryTaskError::NotFound(session_id));
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn rename(
        &self,
        session_id: SessionId,
        label: &str,
    ) -> Result<(), RepositoryTaskError> {
        let result = sqlx::query("UPDATE tasks SET label = ?, updated_at = ? WHERE session_id = ?")
            .bind(label)
            .bind(Utc::now().to_rfc3339())
            .bind(session_id.to_string())
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(RepositoryTaskError::NotFound(session_id));
        }
        Ok(())
    }

    pub async fn reconcile_worktrees(
        &self,
        worktrees: &[WorktreeRecord],
    ) -> Result<(), RepositoryTaskError> {
        let tasks = self.tasks().await?;
        let now = Utc::now().to_rfc3339();
        let mut transaction = self.pool.begin().await?;
        for task in tasks
            .into_iter()
            .filter(|task| task.lifecycle != SessionLifecycle::Archived)
        {
            let valid = worktrees.iter().any(|worktree| {
                same_path(&worktree.path, &task.workspace)
                    && worktree.branch.as_deref() == Some(task.branch.as_str())
            });
            let lifecycle = if valid { "active" } else { "unavailable" };
            sqlx::query("UPDATE tasks SET lifecycle = ?, updated_at = ? WHERE session_id = ?")
                .bind(lifecycle)
                .bind(&now)
                .bind(task.session_id.to_string())
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        Ok(())
    }
}

/// Lease-conflict detail an operator can act on: which process, since when,
/// and from where. Falls back to a plain statement if the lock file could not
/// be read (a partially written or truncated owner record).
fn describe_owner(owner: &Option<LeaseOwner>) -> String {
    match owner {
        Some(owner) => format!(
            "pid {}, started {}, workspace {}",
            owner.pid,
            owner.started_at.to_rfc3339(),
            owner.workspace.display()
        ),
        None => "owner unknown".into(),
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn parse_session_id(value: String) -> Result<SessionId, RepositoryTaskError> {
    SessionId::parse_str(&value).map_err(|_| RepositoryTaskError::InvalidSessionId(value))
}

fn parse_timestamp(value: String) -> Result<DateTime<Utc>, RepositoryTaskError> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| RepositoryTaskError::InvalidStoredValue {
            field: "timestamp",
            value,
        })
}

fn parse_task(row: &sqlx::sqlite::SqliteRow) -> Result<RepositoryTask, RepositoryTaskError> {
    Ok(RepositoryTask {
        session_id: parse_session_id(row.get("session_id"))?,
        label: row.get("label"),
        workspace: PathBuf::from(row.get::<String, _>("workspace")),
        branch: row.get("branch"),
        ownership: WorktreeOwnership::parse(row.get::<String, _>("ownership").as_str())?,
        lifecycle: SessionLifecycle::parse(row.get::<String, _>("lifecycle").as_str())?,
        turn_state: SupervisorTurnState::parse(row.get::<String, _>("turn_state").as_str())?,
        slot: row
            .get::<Option<i64>, _>("slot")
            .and_then(|value| u8::try_from(value).ok()),
        model_id: row.get("model_id"),
        route_id: row.get("route_id"),
        reasoning_effort: row.get("reasoning_effort"),
        created_at: parse_timestamp(row.get("created_at"))?,
        updated_at: parse_timestamp(row.get("updated_at"))?,
        archived_at: row
            .get::<Option<String>, _>("archived_at")
            .map(parse_timestamp)
            .transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn new_task(workspace: &Path, label: &str, slot: Option<u8>) -> NewRepositoryTask {
        NewRepositoryTask {
            session_id: SessionId::new_v4(),
            label: label.into(),
            workspace: workspace.to_path_buf(),
            branch: format!("forge/{label}"),
            ownership: WorktreeOwnership::Managed,
            slot,
            model_id: "mock".into(),
            route_id: "native".into(),
            reasoning_effort: Some("high".into()),
        }
    }

    #[tokio::test]
    async fn one_active_task_per_worktree_and_slot() {
        let dir = TempDir::new().unwrap();
        let control = RepositoryControl::open(dir.path()).await.unwrap();
        let first = new_task(&dir.path().join("a"), "a", Some(1));
        let first_id = first.session_id;
        control.register_task(first, None).await.unwrap();

        let same_workspace = new_task(&dir.path().join("a"), "b", Some(2));
        assert!(control.register_task(same_workspace, None).await.is_err());
        let same_slot = new_task(&dir.path().join("b"), "b", Some(1));
        assert!(control.register_task(same_slot, None).await.is_err());

        control.archive(first_id).await.unwrap();
        control
            .register_task(
                new_task(&dir.path().join("a"), "replacement", Some(1)),
                None,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn running_waiting_and_queued_tasks_cannot_archive() {
        let dir = TempDir::new().unwrap();
        let control = RepositoryControl::open(dir.path()).await.unwrap();
        let task = new_task(&dir.path().join("a"), "a", None);
        let session_id = task.session_id;
        control.register_task(task, None).await.unwrap();
        for state in [
            SupervisorTurnState::Queued,
            SupervisorTurnState::Running,
            SupervisorTurnState::Waiting,
        ] {
            control.set_turn_state(session_id, state).await.unwrap();
            assert!(matches!(
                control.archive(session_id).await,
                Err(RepositoryTaskError::MustStopBeforeArchive(id)) if id == session_id
            ));
        }
        control
            .set_turn_state(session_id, SupervisorTurnState::Cancelled)
            .await
            .unwrap();
        control.archive(session_id).await.unwrap();
    }

    #[tokio::test]
    async fn pin_conflicts_require_an_explicit_swap() {
        let dir = TempDir::new().unwrap();
        let control = RepositoryControl::open(dir.path()).await.unwrap();
        let first = new_task(&dir.path().join("a"), "a", Some(1));
        let second = new_task(&dir.path().join("b"), "b", Some(2));
        let first_id = first.session_id;
        let second_id = second.session_id;
        control.register_task(first, None).await.unwrap();
        control.register_task(second, None).await.unwrap();

        assert!(matches!(
            control.assign_slot(second_id, Some(1), false).await,
            Err(RepositoryTaskError::SlotOccupied { slot: 1, session_id })
                if session_id == first_id
        ));
        control.assign_slot(second_id, Some(1), true).await.unwrap();
        assert_eq!(control.task(first_id).await.unwrap().slot, None);
        assert_eq!(control.task(second_id).await.unwrap().slot, Some(1));
    }

    #[tokio::test]
    async fn pending_creation_and_task_registration_are_recoverable() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        let control = RepositoryControl::open(dir.path()).await.unwrap();
        let pending = control
            .begin_managed_creation("parser", &source, None)
            .await
            .unwrap();
        control
            .mark_worktree_created(pending.operation_id, &target, "forge/parser-1")
            .await
            .unwrap();
        let task = new_task(&target, "parser", None);
        control
            .register_task(task, Some(pending.operation_id))
            .await
            .unwrap();

        let status = sqlx::query("SELECT status FROM pending_operations WHERE operation_id = ?")
            .bind(pending.operation_id as i64)
            .fetch_one(&control.pool)
            .await
            .unwrap()
            .get::<String, _>("status");
        assert_eq!(status, "awaiting_trust");
        control
            .complete_creation(pending.operation_id)
            .await
            .unwrap();
        let status = sqlx::query("SELECT status FROM pending_operations WHERE operation_id = ?")
            .bind(pending.operation_id as i64)
            .fetch_one(&control.pool)
            .await
            .unwrap()
            .get::<String, _>("status");
        assert_eq!(status, "completed");
    }

    #[tokio::test]
    async fn cancelled_creation_returns_target_and_records_reason() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        let control = RepositoryControl::open(dir.path()).await.unwrap();
        let pending = control
            .begin_managed_creation("parser", &source, None)
            .await
            .unwrap();
        control
            .mark_worktree_created(pending.operation_id, &target, "forge/parser-1")
            .await
            .unwrap();

        let result = control
            .cancel_creation(pending.operation_id, "trust declined")
            .await
            .unwrap();
        assert_eq!(result.0, Some(target));
        assert_eq!(result.1, None);
        let status =
            sqlx::query("SELECT status, error FROM pending_operations WHERE operation_id = ?")
                .bind(pending.operation_id as i64)
                .fetch_one(&control.pool)
                .await
                .unwrap();
        assert_eq!(status.get::<String, _>("status"), "cancelled");
        assert_eq!(status.get::<String, _>("error"), "trust declined");
    }

    #[tokio::test]
    async fn archived_and_untrusted_tasks_reject_prompt_queueing() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        let control = RepositoryControl::open(dir.path()).await.unwrap();
        let pending = control
            .begin_managed_creation("parser", &source, None)
            .await
            .unwrap();
        control
            .mark_worktree_created(pending.operation_id, &target, "forge/parser-1")
            .await
            .unwrap();
        let task = new_task(&target, "parser", None);
        let session_id = task.session_id;
        control
            .register_task(task, Some(pending.operation_id))
            .await
            .unwrap();
        assert!(matches!(
            control.enqueue_prompt(session_id, "run it").await,
            Err(RepositoryTaskError::TrustRequired(id)) if id == session_id
        ));
        control
            .complete_creation(pending.operation_id)
            .await
            .unwrap();
        control
            .set_turn_state(session_id, SupervisorTurnState::Cancelled)
            .await
            .unwrap();
        control.archive(session_id).await.unwrap();
        assert!(matches!(
            control.enqueue_prompt(session_id, "run it").await,
            Err(RepositoryTaskError::Archived(id)) if id == session_id
        ));
    }

    #[tokio::test]
    async fn a_first_prompt_is_parked_until_trust_completes_the_creation() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        let control = RepositoryControl::open(dir.path()).await.unwrap();
        let pending = control
            .begin_managed_creation("parser", &source, Some("rewrite the lexer"))
            .await
            .unwrap();
        assert_eq!(pending.first_prompt.as_deref(), Some("rewrite the lexer"));
        control
            .mark_worktree_created(pending.operation_id, &target, "forge/parser-1")
            .await
            .unwrap();
        let task = new_task(&target, "parser", None);
        let session_id = task.session_id;
        control
            .register_task(task, Some(pending.operation_id))
            .await
            .unwrap();

        // Nothing is queued while the task awaits trust.
        assert!(control.queued_prompts(session_id).await.unwrap().is_empty());
        assert!(matches!(
            control.enqueue_prompt(session_id, "rewrite the lexer").await,
            Err(RepositoryTaskError::TrustRequired(id)) if id == session_id
        ));

        let completed = control
            .complete_creation(pending.operation_id)
            .await
            .unwrap();
        assert_eq!(completed.session_id, Some(session_id));
        assert_eq!(completed.first_prompt.as_deref(), Some("rewrite the lexer"));
        control
            .enqueue_prompt(session_id, &completed.first_prompt.unwrap())
            .await
            .unwrap();
        assert_eq!(
            control
                .queued_prompts(session_id)
                .await
                .unwrap()
                .into_iter()
                .map(|(_, text)| text)
                .collect::<Vec<_>>(),
            vec!["rewrite the lexer".to_string()]
        );
    }

    #[tokio::test]
    async fn interrupted_creations_are_reported_as_stale_until_resolved() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        let control = RepositoryControl::open(dir.path()).await.unwrap();
        let pending = control
            .begin_managed_creation("parser", &source, None)
            .await
            .unwrap();
        control
            .mark_worktree_created(pending.operation_id, &target, "forge/parser-1")
            .await
            .unwrap();
        let task = new_task(&target, "parser", None);
        let session_id = task.session_id;
        control
            .register_task(task, Some(pending.operation_id))
            .await
            .unwrap();

        let stale = control.stale_creations().await.unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].operation_id, pending.operation_id);
        assert_eq!(stale[0].workspace.as_deref(), Some(target.as_path()));
        assert_eq!(stale[0].session_id, Some(session_id));

        control
            .cancel_creation(pending.operation_id, "interrupted")
            .await
            .unwrap();
        assert!(control.stale_creations().await.unwrap().is_empty());
        assert!(matches!(
            control.task(session_id).await,
            Err(RepositoryTaskError::NotFound(id)) if id == session_id
        ));
    }

    #[tokio::test]
    async fn attaching_the_same_worktree_twice_names_the_live_task() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("linked");
        let control = RepositoryControl::open(dir.path()).await.unwrap();
        let first = new_task(&workspace, "linked", None);
        let first_id = first.session_id;
        control.register_task(first, None).await.unwrap();

        let duplicate = new_task(&workspace, "linked-again", None);
        assert!(matches!(
            control.register_task(duplicate, None).await,
            Err(RepositoryTaskError::WorkspaceInUse { session_id, .. }) if session_id == first_id
        ));
    }

    #[tokio::test]
    async fn a_control_database_without_first_prompt_migrates_in_place() {
        let dir = TempDir::new().unwrap();
        {
            // Stand up the pre-`first_prompt` shape, then reopen through the
            // real migration path.
            let control = RepositoryControl::open(dir.path()).await.unwrap();
            sqlx::query("ALTER TABLE pending_operations DROP COLUMN first_prompt")
                .execute(&control.pool)
                .await
                .unwrap();
        }
        let control = RepositoryControl::open(dir.path()).await.unwrap();
        let pending = control
            .begin_managed_creation("parser", &dir.path().join("source"), Some("go"))
            .await
            .unwrap();
        assert_eq!(pending.first_prompt.as_deref(), Some("go"));
    }

    #[test]
    fn a_lease_conflict_names_the_owning_process() {
        let dir = TempDir::new().unwrap();
        let _held = RepositoryLease::acquire(dir.path(), Path::new("/repo")).unwrap();
        let error = RepositoryLease::acquire(dir.path(), Path::new("/repo")).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains(&format!("pid {}", std::process::id())),
            "lease conflict should name the owner: {message}"
        );
        assert!(
            message.contains("/repo"),
            "lease conflict should name the owning workspace: {message}"
        );
    }

    #[tokio::test]
    async fn reconciliation_marks_branch_or_path_drift_unavailable_and_recovers() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("task");
        std::fs::create_dir_all(&workspace).unwrap();
        let control = RepositoryControl::open(dir.path()).await.unwrap();
        let task = new_task(&workspace, "parser", None);
        let session_id = task.session_id;
        let branch = task.branch.clone();
        control.register_task(task, None).await.unwrap();

        control.reconcile_worktrees(&[]).await.unwrap();
        assert_eq!(
            control.task(session_id).await.unwrap().lifecycle,
            SessionLifecycle::Unavailable
        );
        control
            .reconcile_worktrees(&[WorktreeRecord {
                path: workspace,
                branch: Some(branch),
                head: None,
                prunable: false,
            }])
            .await
            .unwrap();
        assert_eq!(
            control.task(session_id).await.unwrap().lifecycle,
            SessionLifecycle::Active
        );
    }

    #[test]
    fn exclusive_lease_reports_the_live_owner_and_recovers_after_drop() {
        let dir = TempDir::new().unwrap();
        let first = RepositoryLease::acquire(dir.path(), Path::new("/repo")).unwrap();
        let error = RepositoryLease::acquire(dir.path(), Path::new("/repo")).unwrap_err();
        assert!(matches!(
            error,
            RepositoryTaskError::AlreadyOwned { owner: Some(ref owner), .. }
                if owner.pid == std::process::id()
        ));
        drop(first);
        RepositoryLease::acquire(dir.path(), Path::new("/repo")).unwrap();
    }
}
