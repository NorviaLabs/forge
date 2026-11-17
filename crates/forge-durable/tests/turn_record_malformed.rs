//! A turn record that cannot be decoded is an error, never a panic: a
//! corrupted or hand-made journal must not take down whatever reads it (the
//! model report skips and lists such a journal).

use std::path::{Path, PathBuf};

use forge_durable::{new_session_id, read_turn_records, Journal};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection};

const FULL_TABLE: &str = r#"CREATE TABLE turn_records (
    seq INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, parent_session_id TEXT,
    task_id INTEGER, started_at TEXT, ended_at TEXT, model TEXT, profile_id TEXT,
    reasoning_effort TEXT, tier TEXT, outcome TEXT, completion_reason TEXT,
    escalated INTEGER, escalation_retry INTEGER, prompt_tokens INTEGER,
    completion_tokens INTEGER, cache_read_tokens INTEGER, cache_write_tokens INTEGER,
    model_calls INTEGER, duration_ms INTEGER, waiting_ms INTEGER, signals TEXT,
    task_kind TEXT
)"#;

/// A row with every column well-formed except those `overrides` replace
/// (column, SQL literal).
fn insert(session_id: &str, overrides: &[(&str, &str)]) -> String {
    let mut columns = vec![
        ("session_id", format!("'{session_id}'")),
        ("task_id", "1".into()),
        ("ended_at", "'2025-06-01T00:00:00+00:00'".into()),
        ("model", "'a/b'".into()),
        ("profile_id", "'a'".into()),
        ("outcome", "'completed'".into()),
        ("completion_reason", "'edit_verified'".into()),
        ("escalated", "0".into()),
        ("escalation_retry", "0".into()),
        ("signals", "'[]'".into()),
        ("task_kind", "'main'".into()),
    ];
    for (column, value) in overrides {
        match columns.iter_mut().find(|(name, _)| name == column) {
            Some(entry) => entry.1 = value.to_string(),
            None => columns.push((column, value.to_string())),
        }
    }
    let names: Vec<&str> = columns.iter().map(|(name, _)| *name).collect();
    let values: Vec<&str> = columns.iter().map(|(_, value)| value.as_str()).collect();
    format!(
        "INSERT INTO turn_records ({}) VALUES ({})",
        names.join(", "),
        values.join(", ")
    )
}

async fn journal_file(dir: &Path, statements: &[String]) -> PathBuf {
    let path = dir.join(format!("{}.db", new_session_id()));
    let mut conn = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .connect()
        .await
        .unwrap();
    for statement in statements {
        sqlx::raw_sql(sqlx::AssertSqlSafe(statement.clone()))
            .execute(&mut conn)
            .await
            .unwrap();
    }
    conn.close().await.unwrap();
    path
}

#[tokio::test]
async fn a_table_missing_columns_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let session = new_session_id().to_string();
    let path = journal_file(
        dir.path(),
        &[
            "CREATE TABLE turn_records (seq INTEGER PRIMARY KEY, session_id TEXT)".into(),
            format!("INSERT INTO turn_records VALUES (1, '{session}')"),
        ],
    )
    .await;
    assert!(read_turn_records(&path).await.is_err());
}

#[tokio::test]
async fn a_value_of_the_wrong_type_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let session = new_session_id().to_string();
    for (column, value) in [
        ("task_id", "'not a number'"),
        ("escalated", "'yes'"),
        ("prompt_tokens", "'many'"),
        ("model", "x'00ff'"),
        ("session_id", "'not-a-uuid'"),
        ("ended_at", "'yesterday'"),
        ("signals", "'not json'"),
    ] {
        let path = journal_file(
            dir.path(),
            &[FULL_TABLE.into(), insert(&session, &[(column, value)])],
        )
        .await;
        assert!(
            read_turn_records(&path).await.is_err(),
            "{column} = {value} must be an error"
        );
    }
}

/// The journal's own reader (used by sessions) fails the same way.
#[tokio::test]
async fn a_journals_own_reader_returns_an_error_for_a_bad_row() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = new_session_id();
    drop(Journal::open(dir.path(), session_id).await.unwrap());
    let path = dir.path().join(format!("{session_id}.db"));
    let mut conn = SqliteConnectOptions::new()
        .filename(&path)
        .connect()
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(insert(
        &session_id.to_string(),
        &[("task_id", "'not a number'")],
    )))
    .execute(&mut conn)
    .await
    .unwrap();
    conn.close().await.unwrap();

    let journal = Journal::open(dir.path(), session_id).await.unwrap();
    assert!(journal.turn_records(session_id).await.is_err());
}
