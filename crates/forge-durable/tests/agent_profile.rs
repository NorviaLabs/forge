//! `read_agent_profile`: what a session was started as, read from its own
//! journal without changing or creating anything — how a restart decides
//! whether an unrecorded subagent may be reopened.

use forge_durable::{new_session_id, read_agent_profile, Journal};
use serde_json::json;

async fn settle(dir: &std::path::Path) {
    for _ in 0..500 {
        let busy = std::fs::read_dir(dir)
            .unwrap()
            .any(|e| e.unwrap().file_name().to_string_lossy().ends_with("-wal"));
        if !busy {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn the_latest_profile_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let id = new_session_id();
    let journal = Journal::open(dir.path(), id).await.unwrap();
    journal
        .append_agent_profile(id, json!({"mode": "writer"}))
        .await
        .unwrap();
    journal
        .append_agent_profile(id, json!({"mode": "read_only", "agent": "reviewer"}))
        .await
        .unwrap();
    drop(journal);
    settle(dir.path()).await;
    let path = dir.path().join(format!("{id}.db"));
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        read_agent_profile(&path, id).await.unwrap(),
        Some(json!({"mode": "read_only", "agent": "reviewer"}))
    );
    assert_eq!(std::fs::read(&path).unwrap(), before, "unchanged");
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
}

#[tokio::test]
async fn a_journal_without_a_profile_has_none() {
    let dir = tempfile::tempdir().unwrap();
    let id = new_session_id();
    drop(Journal::open(dir.path(), id).await.unwrap());
    settle(dir.path()).await;
    let path = dir.path().join(format!("{id}.db"));
    assert_eq!(read_agent_profile(&path, id).await.unwrap(), None);
}

#[tokio::test]
async fn a_missing_journal_is_an_error_and_is_not_created() {
    let dir = tempfile::tempdir().unwrap();
    let id = new_session_id();
    let path = dir.path().join(format!("{id}.db"));
    assert!(read_agent_profile(&path, id).await.is_err());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
