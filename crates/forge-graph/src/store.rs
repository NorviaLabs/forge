//! SQLite-backed symbol/edge store.
//!
//! Build is two-pass, because edge resolution needs the whole repo's symbol
//! table before it can honestly say whether a callee name is unique:
//! `insert_symbols` (pass 1, every file) then `resolve_and_insert_edges`
//! (pass 2, once every file's symbols are in). A callee/import name that
//! matches zero in-repo symbols is simply not recorded — v1 never records a
//! fact about code outside the repo. A name matching more than one symbol
//! gets one edge row per real candidate: ambiguity is a query-time count,
//! never a stored guess (see `schema.rs`'s module doc).

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};

use crate::extract::{EdgeFact, SymbolFact};
use crate::schema::{kind, SCHEMA_SQL};

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("graph database error: {0}")]
    Db(#[from] sqlx::Error),
}

/// One definition, as returned to a caller — same shape whether it came
/// from `find_definition` or as the resolved target of a reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolMatch {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
}

/// One call site referencing a definition, plus whether that same call
/// site also syntactically matched other candidate definitions — the
/// honest "cannot fully disambiguate" signal `find_references` must
/// surface rather than silently pick one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceMatch {
    pub from_file: String,
    pub from_line: u32,
    pub resolved: SymbolMatch,
    pub ambiguous_at_callsite: bool,
}

pub struct GraphStore {
    pool: SqlitePool,
}

impl GraphStore {
    /// Opens (creating if absent) the store at `db_path` and applies the
    /// schema. WAL + a single connection: the watcher (PR3) is the only
    /// writer, tool queries are the only readers, and this idiom mirrors
    /// `forge-durable`'s `Journal::open` for the same reason — one writer,
    /// no `SQLITE_BUSY` contention.
    pub async fn open(db_path: &Path) -> Result<Self, GraphError> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(sqlx::Error::Io)?;
        }
        let opts = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        sqlx::query(SCHEMA_SQL).execute(&pool).await?;
        Ok(Self { pool })
    }

    /// In-memory store, for tests that don't need a real file.
    pub async fn open_in_memory() -> Result<Self, GraphError> {
        let opts: SqliteConnectOptions = "sqlite::memory:".parse()?;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        sqlx::query(SCHEMA_SQL).execute(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn insert_symbols(
        &self,
        lang: &str,
        symbols: &[SymbolFact],
    ) -> Result<(), GraphError> {
        let mut tx = self.pool.begin().await?;
        for s in symbols {
            sqlx::query(
                "INSERT INTO symbols (name, qualified, kind, lang, file, line, col) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&s.name)
            .bind(&s.qualified)
            .bind(s.kind)
            .bind(lang)
            .bind(&s.file)
            .bind(s.line as i64)
            .bind(s.col as i64)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Pass 2: resolve every buffered edge's `target_name` against the now-
    /// complete `symbols` table. `src_symbol`, when present, is resolved
    /// within the *same file* only — it names the enclosing symbol this
    /// edge originates from, which is unambiguous by construction (it's the
    /// function/type the extractor was inside when it saw this reference).
    pub async fn resolve_and_insert_edges(&self, edges: &[EdgeFact]) -> Result<(), GraphError> {
        let mut tx = self.pool.begin().await?;
        for e in edges {
            let src_symbol_id: Option<i64> = if let Some(src_name) = &e.src_symbol {
                sqlx::query("SELECT id FROM symbols WHERE name = ? AND file = ? LIMIT 1")
                    .bind(src_name)
                    .bind(&e.from_file)
                    .fetch_optional(&mut *tx)
                    .await?
                    .map(|row| row.get::<i64, _>(0))
            } else {
                None
            };

            let dst_ids: Vec<i64> = sqlx::query("SELECT id FROM symbols WHERE name = ?")
                .bind(&e.target_name)
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .map(|row| row.get::<i64, _>(0))
                .collect();

            for dst_id in dst_ids {
                sqlx::query(
                    "INSERT INTO edges (kind, src_symbol_id, dst_symbol_id, from_file, from_line) \
                     VALUES (?, ?, ?, ?, ?)",
                )
                .bind(e.kind)
                .bind(src_symbol_id)
                .bind(dst_id)
                .bind(&e.from_file)
                .bind(e.from_line as i64)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// Every symbol named `name`. Zero rows means "not found"; more than
    /// one means the name is genuinely ambiguous repo-wide — the caller
    /// (the `find_definition` tool) is responsible for presenting that
    /// honestly rather than picking one.
    pub async fn find_definition(&self, name: &str) -> Result<Vec<SymbolMatch>, GraphError> {
        let rows = sqlx::query(
            "SELECT name, kind, file, line FROM symbols WHERE name = ? ORDER BY file, line",
        )
        .bind(name)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| SymbolMatch {
                name: row.get(0),
                kind: row.get(1),
                file: row.get(2),
                line: row.get::<i64, _>(3) as u32,
            })
            .collect())
    }

    /// Every call site whose target resolves to a symbol named `name`.
    /// `ambiguous_at_callsite` is true when the same `(from_file,
    /// from_line)` also produced an edge to a *different* symbol — i.e.
    /// this call site itself could not be structurally narrowed to one
    /// definition, and `resolved` here is just one of several honest
    /// candidates for it.
    pub async fn find_references(&self, name: &str) -> Result<Vec<ReferenceMatch>, GraphError> {
        let rows = sqlx::query(
            "SELECT e.from_file, e.from_line, s.name, s.kind, s.file, s.line, \
                    (SELECT COUNT(*) FROM edges e2 \
                       WHERE e2.from_file = e.from_file AND e2.from_line = e.from_line \
                         AND e2.kind = e.kind AND e2.dst_symbol_id != e.dst_symbol_id) AS other_candidates \
             FROM edges e JOIN symbols s ON s.id = e.dst_symbol_id \
             WHERE s.name = ? AND e.kind = ? \
             ORDER BY e.from_file, e.from_line",
        )
        .bind(name)
        .bind(kind::CALLS)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| ReferenceMatch {
                from_file: row.get(0),
                from_line: row.get::<i64, _>(1) as u32,
                resolved: SymbolMatch {
                    name: row.get(2),
                    kind: row.get(3),
                    file: row.get(4),
                    line: row.get::<i64, _>(5) as u32,
                },
                ambiguous_at_callsite: row.get::<i64, _>(6) > 0,
            })
            .collect())
    }

    /// Deletes every row (symbols + their outgoing/incoming edges) for one
    /// file — the unit of work an incremental re-extraction (PR3's watcher)
    /// replaces. Exposed now so PR3 doesn't need a store-layer change.
    pub async fn clear_file(&self, file: &str) -> Result<(), GraphError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM edges WHERE dst_symbol_id IN (SELECT id FROM symbols WHERE file = ?) \
                OR from_file = ?",
        )
        .bind(file)
        .bind(file)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM symbols WHERE file = ?")
            .bind(file)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{EdgeFact, SymbolFact};

    fn sym(name: &str, kind: &'static str, file: &str, line: u32) -> SymbolFact {
        SymbolFact {
            name: name.into(),
            qualified: None,
            kind,
            file: file.into(),
            line,
            col: 0,
        }
    }

    #[tokio::test]
    async fn unique_symbol_resolves_with_one_confident_match() {
        let store = GraphStore::open_in_memory().await.unwrap();
        store
            .insert_symbols("rust", &[sym("AgentSession", kind::TYPE, "a.rs", 10)])
            .await
            .unwrap();
        let hits = store.find_definition("AgentSession").await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file, "a.rs");
    }

    #[tokio::test]
    async fn ambiguous_symbol_returns_every_candidate_not_a_guess() {
        let store = GraphStore::open_in_memory().await.unwrap();
        store
            .insert_symbols(
                "rust",
                &[
                    sym("process", kind::METHOD, "a.rs", 1),
                    sym("process", kind::METHOD, "b.rs", 5),
                    sym("process", kind::METHOD, "c.rs", 9),
                ],
            )
            .await
            .unwrap();
        let hits = store.find_definition("process").await.unwrap();
        assert_eq!(
            hits.len(),
            3,
            "every real candidate must be returned, none dropped"
        );
    }

    #[tokio::test]
    async fn unknown_symbol_resolves_to_nothing() {
        let store = GraphStore::open_in_memory().await.unwrap();
        let hits = store.find_definition("NoSuchThing").await.unwrap();
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn a_call_to_an_undefined_name_records_no_edge() {
        let store = GraphStore::open_in_memory().await.unwrap();
        store
            .insert_symbols("rust", &[sym("caller", kind::FUNCTION, "a.rs", 1)])
            .await
            .unwrap();
        store
            .resolve_and_insert_edges(&[EdgeFact {
                kind: kind::CALLS,
                src_symbol: Some("caller".into()),
                target_name: "println".into(), // not an in-repo symbol
                from_file: "a.rs".into(),
                from_line: 2,
            }])
            .await
            .unwrap();
        let refs = store.find_references("println").await.unwrap();
        assert!(
            refs.is_empty(),
            "a call to an unresolvable name must not fabricate an edge"
        );
    }

    #[tokio::test]
    async fn find_references_reports_a_shared_callsite_as_ambiguous() {
        let store = GraphStore::open_in_memory().await.unwrap();
        store
            .insert_symbols(
                "rust",
                &[
                    sym("caller", kind::FUNCTION, "a.rs", 1),
                    sym("process", kind::METHOD, "b.rs", 1),
                    sym("process", kind::METHOD, "c.rs", 1),
                ],
            )
            .await
            .unwrap();
        store
            .resolve_and_insert_edges(&[EdgeFact {
                kind: kind::CALLS,
                src_symbol: Some("caller".into()),
                target_name: "process".into(),
                from_file: "a.rs".into(),
                from_line: 2,
            }])
            .await
            .unwrap();
        let refs = store.find_references("process").await.unwrap();
        assert_eq!(refs.len(), 2, "one edge row per real candidate");
        assert!(refs.iter().all(|r| r.ambiguous_at_callsite));
    }

    #[tokio::test]
    async fn find_references_for_an_unambiguous_call_is_not_flagged() {
        let store = GraphStore::open_in_memory().await.unwrap();
        store
            .insert_symbols(
                "rust",
                &[
                    sym("caller", kind::FUNCTION, "a.rs", 1),
                    sym("callee", kind::FUNCTION, "b.rs", 1),
                ],
            )
            .await
            .unwrap();
        store
            .resolve_and_insert_edges(&[EdgeFact {
                kind: kind::CALLS,
                src_symbol: Some("caller".into()),
                target_name: "callee".into(),
                from_file: "a.rs".into(),
                from_line: 2,
            }])
            .await
            .unwrap();
        let refs = store.find_references("callee").await.unwrap();
        assert_eq!(refs.len(), 1);
        assert!(!refs[0].ambiguous_at_callsite);
    }

    #[tokio::test]
    async fn clear_file_removes_its_symbols_and_touching_edges() {
        let store = GraphStore::open_in_memory().await.unwrap();
        store
            .insert_symbols(
                "rust",
                &[
                    sym("caller", kind::FUNCTION, "a.rs", 1),
                    sym("callee", kind::FUNCTION, "b.rs", 1),
                ],
            )
            .await
            .unwrap();
        store
            .resolve_and_insert_edges(&[EdgeFact {
                kind: kind::CALLS,
                src_symbol: Some("caller".into()),
                target_name: "callee".into(),
                from_file: "a.rs".into(),
                from_line: 2,
            }])
            .await
            .unwrap();
        store.clear_file("b.rs").await.unwrap();
        assert!(store.find_definition("callee").await.unwrap().is_empty());
        assert!(store.find_references("callee").await.unwrap().is_empty());
    }
}
