//! Invariant: a paired client sends a prompt that reaches the session, and
//! approves (or denies) a pending request, as the device.

mod common;

use common::Harness;
use forge_remote::protocol::RemoteDecision;
use forge_remote::testing::{session, with_approval, Decided, TestClient};
use forge_types::SessionId;
use serde_json::json;

const ALL: [RemoteDecision; 3] = [
    RemoteDecision::AllowOnce,
    RemoteDecision::Deny,
    RemoteDecision::DenyWithNote,
];

async fn setup(options: &[RemoteDecision]) -> (Harness, TestClient, String, SessionId) {
    let harness = Harness::new();
    let (_, client, token) = harness.paired().await;
    let session_id = SessionId::new_v4();
    harness
        .backend
        .put(with_approval(session(session_id, "s"), "c1", options));
    (harness, client, token, session_id)
}

#[tokio::test]
async fn a_prompt_reaches_the_session_exactly_once() {
    let (harness, client, token, session_id) = setup(&ALL).await;
    let reply = client
        .post(
            &format!("/v1/sessions/{session_id}/prompt"),
            Some(&token),
            json!({"text": "run the tests"}),
        )
        .await
        .unwrap();
    assert_eq!(reply.status, 202, "{}", reply.text);
    assert_eq!(
        harness.backend.prompts(),
        [(session_id, "run the tests".to_string())]
    );
    let logged = harness
        .activity()
        .into_iter()
        .find(|line| line["event"] == "prompt")
        .expect("the prompt is recorded");
    assert_eq!(logged["device"], "Phone");
    assert_eq!(logged["session_id"], session_id.to_string());
    assert_eq!(logged["chars"], 13);
    assert!(
        !logged.to_string().contains("run the tests"),
        "never the text"
    );
}

#[tokio::test]
async fn an_empty_or_oversized_prompt_is_refused() {
    let (harness, client, token, session_id) = setup(&ALL).await;
    let path = format!("/v1/sessions/{session_id}/prompt");
    for (body, status) in [
        (json!({"text": ""}), 400),
        (json!({"text": "  \n "}), 400),
        (json!({}), 400),
        (json!({"text": "x".repeat(32 * 1024 + 1)}), 413),
    ] {
        let reply = client.post(&path, Some(&token), body).await.unwrap();
        assert_eq!(reply.status, status, "{}", reply.text);
        assert!(reply.body["error"].is_string());
    }
    let reply = client
        .post(&path, Some(&token), json!({"text": "x".repeat(32 * 1024)}))
        .await
        .unwrap();
    assert_eq!(reply.status, 202, "exactly the limit is fine");
    assert_eq!(harness.backend.prompts().len(), 1);
}

#[tokio::test]
async fn a_prompt_for_an_unknown_session_is_not_found() {
    let (harness, client, token, _) = setup(&ALL).await;
    let reply = client
        .post(
            &format!("/v1/sessions/{}/prompt", SessionId::new_v4()),
            Some(&token),
            json!({"text": "hi"}),
        )
        .await
        .unwrap();
    assert_eq!(reply.status, 404);
    assert!(harness.backend.prompts().is_empty());
}

#[tokio::test]
async fn allowing_a_pending_request_reaches_the_session_as_the_device() {
    let (harness, client, token, session_id) = setup(&ALL).await;
    let reply = client
        .post(
            &format!("/v1/sessions/{session_id}/approval"),
            Some(&token),
            json!({"call_id": "c1", "decision": "allow_once"}),
        )
        .await
        .unwrap();
    assert_eq!(reply.status, 202, "{}", reply.text);
    assert_eq!(
        harness.backend.decisions(),
        [Decided {
            session_id,
            call_id: "c1".into(),
            decision: RemoteDecision::AllowOnce,
            note: None,
            actor: "remote:Phone".into(),
        }]
    );
    assert!(harness
        .activity()
        .iter()
        .any(|line| line["event"] == "decision"
            && line["decision"] == "allow_once"
            && line["call_id"] == "c1"
            && line["device"] == "Phone"));
}

#[tokio::test]
async fn denying_with_a_note_needs_the_note() {
    let (harness, client, token, session_id) = setup(&ALL).await;
    let path = format!("/v1/sessions/{session_id}/approval");
    for body in [
        json!({"call_id": "c1", "decision": "deny_with_note"}),
        json!({"call_id": "c1", "decision": "deny_with_note", "note": "   "}),
    ] {
        let reply = client.post(&path, Some(&token), body).await.unwrap();
        assert_eq!(reply.status, 400, "{}", reply.text);
    }
    let too_long = client
        .post(
            &path,
            Some(&token),
            json!({"call_id": "c1", "decision": "deny_with_note", "note": "x".repeat(4 * 1024 + 1)}),
        )
        .await
        .unwrap();
    assert_eq!(too_long.status, 413);
    assert!(harness.backend.decisions().is_empty());
    let reply = client
        .post(
            &path,
            Some(&token),
            json!({"call_id": "c1", "decision": "deny_with_note", "note": "use the staging db"}),
        )
        .await
        .unwrap();
    assert_eq!(reply.status, 202);
    let decided = harness.backend.decisions();
    assert_eq!(decided[0].decision, RemoteDecision::DenyWithNote);
    assert_eq!(decided[0].note.as_deref(), Some("use the staging db"));
}

#[tokio::test]
async fn a_decision_only_the_desktop_can_make_is_forbidden() {
    let (harness, client, token, session_id) =
        setup(&[RemoteDecision::Deny, RemoteDecision::DenyWithNote]).await;
    let reply = client
        .post(
            &format!("/v1/sessions/{session_id}/approval"),
            Some(&token),
            json!({"call_id": "c1", "decision": "allow_once"}),
        )
        .await
        .unwrap();
    assert_eq!(reply.status, 403, "{}", reply.text);
    assert!(harness.backend.decisions().is_empty());
}

#[tokio::test]
async fn a_grant_the_remote_protocol_does_not_know_is_a_bad_request() {
    let (harness, client, token, session_id) = setup(&ALL).await;
    for decision in [
        "allow_host_always",
        "approve_all",
        "trust_program",
        "ALLOW_ONCE",
    ] {
        let reply = client
            .post(
                &format!("/v1/sessions/{session_id}/approval"),
                Some(&token),
                json!({"call_id": "c1", "decision": decision}),
            )
            .await
            .unwrap();
        assert_eq!(reply.status, 400, "{decision}");
    }
    assert!(harness.backend.decisions().is_empty());
}

#[tokio::test]
async fn answering_a_request_that_is_not_pending_is_a_conflict() {
    let (harness, client, token, session_id) = setup(&ALL).await;
    let path = format!("/v1/sessions/{session_id}/approval");
    let stale = client
        .post(
            &path,
            Some(&token),
            json!({"call_id": "old", "decision": "deny"}),
        )
        .await
        .unwrap();
    assert_eq!(stale.status, 409);
    let first = client
        .post(
            &path,
            Some(&token),
            json!({"call_id": "c1", "decision": "deny"}),
        )
        .await
        .unwrap();
    assert_eq!(first.status, 202);
    let again = client
        .post(
            &path,
            Some(&token),
            json!({"call_id": "c1", "decision": "allow_once"}),
        )
        .await
        .unwrap();
    assert_eq!(again.status, 409, "the first answer wins");
    assert_eq!(harness.backend.decisions().len(), 1);
}

#[tokio::test]
async fn stop_reaches_the_session() {
    let (harness, client, token, session_id) = setup(&ALL).await;
    let reply = client
        .send(TestClient::request(
            "POST",
            &format!("/v1/sessions/{session_id}/stop"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(reply.status, 202, "{}", reply.text);
    assert_eq!(harness.backend.stops(), [session_id]);
    assert!(harness
        .activity()
        .iter()
        .any(|line| line["event"] == "stop" && line["session_id"] == session_id.to_string()));
}

#[tokio::test]
async fn the_wrong_method_is_refused() {
    let (harness, client, token, session_id) = setup(&ALL).await;
    for (method, path) in [
        ("GET", format!("/v1/sessions/{session_id}/prompt")),
        ("DELETE", format!("/v1/sessions/{session_id}/stop")),
        ("POST", "/v1/sessions".to_string()),
    ] {
        let reply = client
            .send(TestClient::request(method, &path, Some(&token), None))
            .await
            .unwrap();
        assert_eq!(reply.status, 405, "{method} {path}");
    }
    assert!(harness.backend.prompts().is_empty() && harness.backend.stops().is_empty());
}

#[tokio::test]
async fn a_device_that_acts_too_often_is_slowed_down() {
    let harness = Harness::with(
        forge_remote::Limits {
            max_actions_per_minute: 5,
            ..forge_remote::Limits::default()
        },
        forge_remote::hub::HubConfig::default(),
    );
    let (_, client, token) = harness.paired().await;
    let session_id = SessionId::new_v4();
    harness.backend.put(session(session_id, "s"));
    let prompt = format!("/v1/sessions/{session_id}/prompt");
    for n in 0..4 {
        let reply = client
            .post(
                &prompt,
                Some(&token),
                json!({"text": format!("prompt {n}")}),
            )
            .await
            .unwrap();
        assert_eq!(reply.status, 202);
    }
    let stop = client
        .send(TestClient::request(
            "POST",
            &format!("/v1/sessions/{session_id}/stop"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(stop.status, 202, "stops share the budget");
    let refused = client
        .post(&prompt, Some(&token), json!({"text": "one too many"}))
        .await
        .unwrap();
    assert_eq!(refused.status, 429, "{}", refused.text);
    assert!(refused.headers.contains_key(hyper::header::RETRY_AFTER));
    assert_eq!(
        client
            .get("/v1/sessions", Some(&token))
            .await
            .unwrap()
            .status,
        200,
        "reading is never limited"
    );
    assert_eq!(harness.backend.prompts().len(), 4);

    harness.clock.advance(std::time::Duration::from_secs(60));
    let later = client
        .post(&prompt, Some(&token), json!({"text": "after a minute"}))
        .await
        .unwrap();
    assert_eq!(later.status, 202);
}

#[tokio::test]
async fn a_remote_prompt_waits_for_a_full_queue_to_drain() {
    let harness = Harness::with(
        forge_remote::Limits {
            max_remote_queue: 3,
            ..forge_remote::Limits::default()
        },
        forge_remote::hub::HubConfig::default(),
    );
    let (_, client, token) = harness.paired().await;
    let session_id = SessionId::new_v4();
    let mut busy = session(session_id, "s");
    busy.queue = vec!["a".into(), "b".into(), "c".into()];
    harness.backend.put(busy.clone());
    let prompt = format!("/v1/sessions/{session_id}/prompt");
    let refused = client
        .post(&prompt, Some(&token), json!({"text": "a fourth"}))
        .await
        .unwrap();
    assert_eq!(refused.status, 429, "{}", refused.text);
    assert!(harness.backend.prompts().is_empty());
    busy.queue.pop();
    harness.backend.put(busy);
    let accepted = client
        .post(&prompt, Some(&token), json!({"text": "a third"}))
        .await
        .unwrap();
    assert_eq!(accepted.status, 202);
}

#[tokio::test]
async fn an_internal_failure_reaches_the_device_without_its_details() {
    let (harness, client, token, session_id) = setup(&ALL).await;
    harness
        .backend
        .fail_prompts(Some(forge_remote::BackendError::Unavailable(
            "session assembly failed: /Users/priya/secret-client/repo is locked".into(),
        )));
    let reply = client
        .post(
            &format!("/v1/sessions/{session_id}/prompt"),
            Some(&token),
            json!({"text": "hi"}),
        )
        .await
        .unwrap();
    assert_eq!(reply.status, 503);
    assert!(!reply.text.contains("/Users"), "{}", reply.text);
    assert!(!reply.text.contains("secret-client"), "{}", reply.text);
}
