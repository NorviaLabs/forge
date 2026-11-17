//! Invariant: a paired client receives a session's events in order, and a
//! reconnect resumes without a silent gap.

mod common;

use std::time::Duration;

use common::Harness;
use forge_remote::hub::HubConfig;
use forge_remote::protocol::RemoteDecision;
use forge_remote::testing::{session, with_approval};
use forge_remote::Limits;
use forge_types::SessionId;
use serde_json::json;

async fn followed(harness: &Harness) -> (forge_remote::testing::TestClient, String, SessionId) {
    let (_, client, token) = harness.paired().await;
    let session_id = SessionId::new_v4();
    harness.backend.put(session(session_id, "Fix flaky test"));
    (client, token, session_id)
}

#[tokio::test]
async fn a_followed_session_streams_every_event_in_order() {
    let harness = Harness::new();
    let (client, token, session_id) = followed(&harness).await;
    let mut stream = client
        .events(session_id, Some(&token), None)
        .await
        .unwrap_or_else(|r| panic!("{} {}", r.status, r.text));
    assert_eq!(
        stream
            .headers
            .get(hyper::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
    assert_eq!(
        stream
            .headers
            .get(hyper::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );

    let first = stream.next().await.expect("the session comes first");
    assert_eq!(first.event, "session");
    assert_eq!(first.seq(), 1);
    assert_eq!(first.data["type"], "session");
    assert_eq!(first.data["label"], "Fix flaky test");
    assert_eq!(first.data["resync"], false);
    let epoch = first.epoch().to_string();

    for i in 0..200 {
        harness.backend.text(session_id, &format!("t{i} "));
    }
    let mut waiting = with_approval(
        session(session_id, "Fix flaky test"),
        "c1",
        &[RemoteDecision::Deny],
    );
    waiting.turn_state = "waiting".into();
    harness.backend.update(waiting);
    harness
        .backend
        .notice(session_id, "Fix flaky test needs you");

    // Streamed text may arrive batched: every character, in order, with
    // ids that never skip, before the update that followed it.
    let sent: String = (0..200).map(|i| format!("t{i} ")).collect();
    let mut received = String::new();
    let mut seq = 1;
    let update = loop {
        let event = stream.next().await.expect("every event arrives");
        seq += 1;
        assert_eq!(event.id, format!("{epoch}.{seq}"), "ids never skip");
        if event.event != "text" {
            break event;
        }
        received.push_str(event.data["delta"].as_str().unwrap());
    };
    assert_eq!(received, sent);
    assert_eq!(update.event, "session");
    assert_eq!(update.data["pending_approval"]["call_id"], "c1");
    let notice = stream.next().await.unwrap();
    assert_eq!((notice.event.as_str(), notice.seq()), ("notice", seq + 1));
    assert_eq!(notice.data["kind"], "attention");
}

#[tokio::test]
async fn another_sessions_events_never_appear() {
    let harness = Harness::new();
    let (client, token, session_id) = followed(&harness).await;
    let other = SessionId::new_v4();
    harness.backend.put(session(other, "other"));
    let mut stream = client.events(session_id, Some(&token), None).await.unwrap();
    stream.next().await.unwrap();
    harness.backend.text(other, "not yours");
    harness.backend.text(session_id, "yours");
    let event = stream.next().await.unwrap();
    assert_eq!(event.data["delta"], "yours");
    assert_eq!(event.seq(), 2);
}

/// One event per delta, for tests that count them.
fn unbatched() -> HubConfig {
    HubConfig {
        text_batch: Duration::ZERO,
        ..HubConfig::default()
    }
}

#[tokio::test]
async fn a_reconnect_replays_exactly_the_missed_events() {
    let harness = Harness::with(Limits::default(), unbatched());
    let (client, token, session_id) = followed(&harness).await;
    let mut stream = client.events(session_id, Some(&token), None).await.unwrap();
    stream.next().await.unwrap();
    for i in 0..10 {
        harness.backend.text(session_id, &format!("a{i}"));
    }
    let mut last = String::new();
    for _ in 0..10 {
        last = stream.next().await.unwrap().id;
    }
    drop(stream);
    for i in 0..5 {
        harness.backend.text(session_id, &format!("b{i}"));
    }
    let mut resumed = client
        .events(session_id, Some(&token), Some(&last))
        .await
        .unwrap();
    for i in 0..5u64 {
        let event = resumed
            .next()
            .await
            .expect("the missed events are replayed");
        assert_eq!(event.event, "text", "no snapshot first: nothing was lost");
        assert_eq!(event.seq(), 12 + i);
        assert_eq!(event.data["delta"], format!("b{i}"));
    }
}

#[tokio::test]
async fn a_reconnect_the_log_cannot_serve_starts_over_with_a_marked_snapshot() {
    let harness = Harness::with(
        Limits::default(),
        HubConfig {
            max_events: 8,
            ..unbatched()
        },
    );
    let (client, token, session_id) = followed(&harness).await;
    let mut stream = client.events(session_id, Some(&token), None).await.unwrap();
    let first = stream.next().await.unwrap();
    drop(stream);
    for i in 0..50 {
        harness.backend.text(session_id, &format!("t{i}"));
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    for resume in [
        first.id.clone(),
        format!("0000000000000000.{}", first.seq()),
        "garbage".to_string(),
    ] {
        let mut resumed = client
            .events(session_id, Some(&token), Some(&resume))
            .await
            .unwrap();
        let event = resumed.next().await.unwrap();
        assert_eq!(event.event, "session", "{resume}");
        assert_eq!(event.data["resync"], true, "{resume}");
    }
}

#[tokio::test]
async fn the_session_list_shows_what_can_be_followed() {
    let harness = Harness::new();
    let (client, token, session_id) = followed(&harness).await;
    let waiting = SessionId::new_v4();
    harness.backend.put(with_approval(
        session(waiting, "Needs you"),
        "c9",
        &[RemoteDecision::Deny],
    ));
    let reply = client.get("/v1/sessions", Some(&token)).await.unwrap();
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.body,
        json!({"sessions": [
            {"session_id": session_id, "project": "project", "label": "Fix flaky test",
             "turn_state": "idle", "needs_approval": false},
            {"session_id": waiting, "project": "project", "label": "Needs you",
             "turn_state": "waiting", "needs_approval": true}
        ]})
    );
}

#[tokio::test]
async fn an_unknown_session_is_not_found() {
    let harness = Harness::new();
    let (_, client, token) = harness.paired().await;
    let refused = client
        .events(SessionId::new_v4(), Some(&token), None)
        .await
        .err()
        .expect("no stream for an unknown session");
    assert_eq!(refused.status, 404);
    let bad_id = client
        .get("/v1/sessions/not-a-uuid/events", Some(&token))
        .await
        .unwrap();
    assert_eq!(bad_id.status, 404);
}

#[tokio::test]
async fn a_session_that_goes_away_ends_its_stream() {
    let harness = Harness::new();
    let (client, token, session_id) = followed(&harness).await;
    let mut stream = client.events(session_id, Some(&token), None).await.unwrap();
    stream.next().await.unwrap();
    harness.backend.remove(session_id);
    let gone = stream.next().await.unwrap();
    assert_eq!(gone.event, "gone");
    assert!(stream.ends_within(Duration::from_secs(5)).await);
}

#[tokio::test]
async fn an_idle_stream_gets_keep_alive_comments() {
    let harness = Harness::with(
        Limits {
            keep_alive: Duration::from_millis(100),
            ..Limits::default()
        },
        HubConfig::default(),
    );
    let (client, token, session_id) = followed(&harness).await;
    let mut stream = client.events(session_id, Some(&token), None).await.unwrap();
    stream.next().await.unwrap();
    assert_eq!(stream.next_within(Duration::from_millis(600)).await, None);
    assert!(stream.comments() >= 2, "{} keep-alives", stream.comments());
}

#[tokio::test]
async fn at_most_four_streams_are_open_at_once() {
    let harness = Harness::new();
    let (client, token, session_id) = followed(&harness).await;
    let mut open = Vec::new();
    for _ in 0..4 {
        let mut stream = client.events(session_id, Some(&token), None).await.unwrap();
        stream.next().await.unwrap();
        open.push(stream);
    }
    let refused = client
        .events(session_id, Some(&token), None)
        .await
        .err()
        .expect("a fifth stream is refused");
    assert_eq!(refused.status, 429);
    open.pop();
    let mut reopened = None;
    for _ in 0..100 {
        if let Ok(stream) = client.events(session_id, Some(&token), None).await {
            reopened = Some(stream);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(reopened.is_some(), "a closed stream frees its place");
}

#[tokio::test]
async fn older_messages_are_fetched_a_page_at_a_time() {
    let harness = Harness::new();
    let (_, client, token) = harness.paired().await;
    let session_id = SessionId::new_v4();
    let mut long = session(session_id, "long");
    long.messages = (0..80)
        .map(|i| forge_remote::protocol::RemoteMessage {
            role: "user".into(),
            name: None,
            text: format!("m{i}"),
            attachments: 0,
        })
        .collect();
    harness.backend.put(long);
    let path = |query: &str| format!("/v1/sessions/{session_id}/messages{query}");

    let page = client
        .get(&path("?before=50&limit=20"), Some(&token))
        .await
        .unwrap();
    assert_eq!(page.status, 200, "{}", page.text);
    assert_eq!(page.body["message_offset"], 30);
    assert_eq!(page.body["message_total"], 80);
    let texts: Vec<&str> = page.body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts.len(), 20);
    assert_eq!((texts[0], texts[19]), ("m30", "m49"));

    let latest = client.get(&path(""), Some(&token)).await.unwrap();
    assert_eq!(
        latest.body["message_offset"], 50,
        "the default is the newest 30"
    );

    for bad in ["?limit=abc", "?before=-1", "?before=1&before=2x"] {
        let reply = client.get(&path(bad), Some(&token)).await.unwrap();
        assert_eq!(reply.status, 400, "{bad}");
    }
    let unknown = client
        .get(
            &format!("/v1/sessions/{}/messages", SessionId::new_v4()),
            Some(&token),
        )
        .await
        .unwrap();
    assert_eq!(unknown.status, 404);
    let unauthorized = client.get(&path(""), None).await.unwrap();
    assert_eq!(unauthorized.status, 401);
}
