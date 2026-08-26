//! SQLite schema for the persisted symbol graph.
//!
//! Three tables: `symbols` (one row per definition), `edges` (one row per
//! syntactic fact relating two symbols, or a symbol and a call site), and
//! `files` (bookkeeping for incremental re-extraction — see `store.rs`).
//!
//! `edges.confidence` is always `'syntactic'` in v1 — every row is a real,
//! tree-sitter-verified fact, never a guess. It's a real column (not a
//! comment-only note) so a future `INFERRED` tier is a migration, not a
//! schema rewrite; nothing in v1 ever writes anything but `'syntactic'`.

pub const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS symbols (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    qualified   TEXT,
    kind        TEXT NOT NULL,
    lang        TEXT NOT NULL,
    file        TEXT NOT NULL,
    line        INTEGER NOT NULL,
    col         INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file);

CREATE TABLE IF NOT EXISTS edges (
    id            INTEGER PRIMARY KEY,
    kind          TEXT NOT NULL,
    src_symbol_id INTEGER REFERENCES symbols(id),
    dst_symbol_id INTEGER NOT NULL REFERENCES symbols(id),
    from_file     TEXT NOT NULL,
    from_line     INTEGER NOT NULL,
    confidence    TEXT NOT NULL DEFAULT 'syntactic'
);
CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst_symbol_id);
CREATE INDEX IF NOT EXISTS idx_edges_src ON edges(src_symbol_id);
CREATE INDEX IF NOT EXISTS idx_edges_callsite ON edges(from_file, from_line, kind);

CREATE TABLE IF NOT EXISTS files (
    path         TEXT PRIMARY KEY,
    lang         TEXT NOT NULL,
    content_hash TEXT NOT NULL
);
"#;

/// Symbol/edge kinds as stored — plain strings rather than a SQL enum so a
/// future kind never requires a migration, only a new match arm here.
pub mod kind {
    pub const FUNCTION: &str = "function";
    pub const METHOD: &str = "method";
    pub const TYPE: &str = "type";

    pub const IMPORTS: &str = "imports";
    pub const IMPLEMENTS: &str = "implements";
    pub const CALLS: &str = "calls";
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::Executor;
    use std::str::FromStr;

    #[tokio::test]
    async fn schema_applies_cleanly_and_is_idempotent() {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:").unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        pool.execute(SCHEMA_SQL).await.unwrap();
        // Applying twice must not error — CREATE TABLE/INDEX IF NOT EXISTS.
        pool.execute(SCHEMA_SQL).await.unwrap();
    }
}
