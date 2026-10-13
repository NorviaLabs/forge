use forge_durable::{new_session_id, Journal, ReplayState};
use forge_types::{Message, MessageRole, SessionId};
use serde_json::json;
use tempfile::TempDir;

async fn wait_for_checkpointed_replay(journal: &Journal, sid: SessionId) -> ReplayState {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let state = journal.replay(sid).await.expect("replay succeeds");
            if state.last_seq >= 256 && state.events.is_empty() {
                return state;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("checkpointed replay becomes visible")
}

fn user_message(content: &str) -> Message {
    Message {
        outcome: Default::default(),
        role: MessageRole::User,
        content: content.into(),
        tool_call_id: None,
        name: None,
        thinking: None,
        thinking_duration_secs: None,
        tool_calls: vec![],
        attachments: Vec::new(),
    }
}

#[tokio::test]
async fn replay_uses_automatic_checkpoint_and_only_materializes_tail_events() {
    let dir = TempDir::new().expect("temp dir");
    let sid = new_session_id();
    let journal = Journal::open(dir.path(), sid).await.expect("open journal");

    for index in 0..256 {
        journal
            .append_user_message(sid, &format!("message {index}"))
            .await
            .expect("append message");
    }

    let checkpointed = wait_for_checkpointed_replay(&journal, sid).await;
    assert_eq!(checkpointed.user_messages.len(), 256);
    assert_eq!(checkpointed.last_seq, 256);

    journal
        .append_user_message(sid, "tail")
        .await
        .expect("append tail");
    let resumed = journal.replay(sid).await.expect("replay with tail");

    assert_eq!(resumed.user_messages.len(), 257);
    assert_eq!(
        resumed.user_messages.last().map(String::as_str),
        Some("tail")
    );
    assert_eq!(resumed.last_seq, 257);
    assert_eq!(
        resumed.events.len(),
        1,
        "only post-checkpoint events are exposed"
    );
}

#[tokio::test]
async fn checkpointed_replay_restores_compacted_projection_and_context_state() {
    let dir = TempDir::new().expect("temp dir");
    let sid = new_session_id();
    let journal = Journal::open(dir.path(), sid).await.expect("open journal");

    journal
        .append_session_created(sid)
        .await
        .expect("session created");
    for index in 0..254 {
        journal
            .append_user_message(sid, &format!("message {index}"))
            .await
            .expect("append message");
    }
    journal
        .append_context_compacted(
            sid,
            json!({
                "messages": [user_message("summary after compaction")],
                "context_state": {"anchor_seq": 255, "summary_tokens": 42},
            }),
        )
        .await
        .expect("append compaction");

    let checkpointed = wait_for_checkpointed_replay(&journal, sid).await;
    assert_eq!(checkpointed.last_seq, 256);
    assert_eq!(checkpointed.messages.len(), 1);
    assert_eq!(checkpointed.messages[0].content, "summary after compaction");
    assert_eq!(
        checkpointed.context_state,
        Some(json!({"anchor_seq": 255, "summary_tokens": 42}))
    );
    assert_eq!(checkpointed.compaction_count, 1);

    journal
        .append_user_message(sid, "after checkpoint")
        .await
        .expect("append tail");
    let resumed = journal.replay(sid).await.expect("replay with tail");

    assert_eq!(resumed.events.len(), 1);
    assert_eq!(resumed.messages.len(), 2);
    assert_eq!(resumed.messages[0].content, "summary after compaction");
    assert_eq!(resumed.messages[1].content, "after checkpoint");
    assert_eq!(resumed.context_state, checkpointed.context_state);
    assert_eq!(resumed.compaction_count, 1);
}
