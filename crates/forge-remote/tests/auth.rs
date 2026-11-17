//! Invariant: requests without a token, with a wrong token, or with a
//! revoked one are refused — all alike — and change nothing.

mod common;

use std::time::Duration;

use common::Harness;
use forge_remote::protocol::RemoteDecision;
use forge_remote::testing::{session, with_approval, TestClient};
use forge_remote::token::DeviceToken;
use forge_types::SessionId;
use serde_json::json;

/// Every route that needs a token, as (method, path, body).
fn protected(session_id: SessionId) -> Vec<(&'static str, String, Option<serde_json::Value>)> {
    vec![
        ("GET", "/v1/hello".into(), None),
        ("GET", "/v1/sessions".into(), None),
        ("GET", format!("/v1/sessions/{session_id}/events"), None),
        (
            "GET",
            format!("/v1/sessions/{session_id}/messages?limit=5"),
            None,
        ),
        (
            "POST",
            format!("/v1/sessions/{session_id}/prompt"),
            Some(json!({"text": "rm -rf"})),
        ),
        (
            "POST",
            format!("/v1/sessions/{session_id}/approval"),
            Some(json!({"call_id": "c1", "decision": "allow_once"})),
        ),
        ("POST", format!("/v1/sessions/{session_id}/stop"), None),
        ("GET", "/v1/no-such-route".into(), None),
    ]
}

/// Send `method path` with the raw `Authorization` header `auth`.
async fn send(
    client: &TestClient,
    method: &str,
    path: &str,
    auth: Option<&str>,
    body: &Option<serde_json::Value>,
) -> forge_remote::testing::Reply {
    let mut request = TestClient::request(
        method,
        path,
        None,
        body.as_ref().map(|b| serde_json::to_vec(b).unwrap()),
    );
    if let Some(auth) = auth {
        request.headers_mut().insert(
            hyper::header::AUTHORIZATION,
            hyper::header::HeaderValue::from_str(auth).unwrap(),
        );
    }
    client.send(request).await.unwrap()
}

fn fixture(harness: &Harness) -> SessionId {
    let session_id = SessionId::new_v4();
    harness.backend.put(with_approval(
        session(session_id, "s"),
        "c1",
        &[RemoteDecision::AllowOnce, RemoteDecision::Deny],
    ));
    session_id
}

fn nothing_reached_the_session(harness: &Harness) {
    assert!(harness.backend.prompts().is_empty());
    assert!(harness.backend.decisions().is_empty());
    assert!(harness.backend.stops().is_empty());
}

#[tokio::test]
async fn a_missing_malformed_or_wrong_token_is_refused_everywhere_alike() {
    let harness = Harness::new();
    let (_, client, token) = harness.paired().await;
    let session_id = fixture(&harness);
    let wrong = DeviceToken::generate();
    let headers: Vec<Option<String>> = vec![
        None,
        Some(String::new()),
        Some("Bearer".into()),
        Some("Bearer ".into()),
        Some("Bearer garbage".into()),
        Some(format!("Basic {token}")),
        Some(token.clone()),
        Some(format!("Bearer {token}x")),
        Some(format!("Bearer {}", wrong.as_str())),
        Some(format!("Bearer  {token} extra")),
    ];
    let mut bodies = std::collections::HashSet::new();
    for (method, path, body) in protected(session_id) {
        for auth in &headers {
            let reply = send(&client, method, &path, auth.as_deref(), &body).await;
            assert_eq!(
                reply.status, 401,
                "{method} {path} with {auth:?}: {}",
                reply.text
            );
            assert_eq!(
                reply
                    .headers
                    .get(hyper::header::WWW_AUTHENTICATE)
                    .and_then(|v| v.to_str().ok()),
                Some("Bearer")
            );
            bodies.insert(reply.text);
        }
    }
    assert_eq!(bodies.len(), 1, "every refusal reads the same: {bodies:?}");
    nothing_reached_the_session(&harness);
}

#[tokio::test]
async fn the_paired_token_is_accepted() {
    let harness = Harness::new();
    let (_, client, token) = harness.paired().await;
    let hello = client.get("/v1/hello", Some(&token)).await.unwrap();
    assert_eq!(hello.status, 200);
    assert_eq!(hello.body, json!({"protocol": 1, "device_name": "Phone"}));
    let unknown = client.get("/v1/no-such-route", Some(&token)).await.unwrap();
    assert_eq!(
        unknown.status, 404,
        "routes are only told apart once authenticated"
    );
}

#[tokio::test]
async fn a_revoked_token_is_refused_everywhere() {
    let harness = Harness::new();
    let (_, client, token) = harness.paired().await;
    let session_id = fixture(&harness);
    harness.service.revoke().await.unwrap();
    assert_eq!(harness.service.status().device, None);
    // Revoking also replaces the certificate, so the old pin fails outright.
    // Even a client that trusts the new certificate gets nowhere with the
    // revoked token.
    let addr = harness.service.status().listening.unwrap();
    assert!(client.tls().await.is_err() || client.addr != addr);
    let client = TestClient::new(addr, harness.pin());
    for (method, path, body) in protected(session_id) {
        let reply = send(
            &client,
            method,
            &path,
            Some(&format!("Bearer {token}")),
            &body,
        )
        .await;
        assert_eq!(reply.status, 401, "{method} {path}");
    }
    nothing_reached_the_session(&harness);
    assert!(harness
        .activity()
        .iter()
        .any(|line| line["event"] == "revoked" && line["device"] == "Phone"));
}

#[tokio::test]
async fn revoking_ends_an_open_stream_at_once() {
    let harness = Harness::new();
    let (_, client, token) = harness.paired().await;
    let session_id = fixture(&harness);
    let mut stream = client.events(session_id, Some(&token), None).await.unwrap();
    stream.next().await.expect("the first snapshot");
    harness.service.revoke().await.unwrap();
    assert!(stream.ends_within(Duration::from_secs(5)).await);
}

#[tokio::test]
async fn a_replaced_device_is_refused_and_disconnected() {
    let harness = Harness::new();
    let (_, client, old) = harness.paired().await;
    let session_id = fixture(&harness);
    let mut stream = client.events(session_id, Some(&old), None).await.unwrap();
    stream.next().await.expect("the first snapshot");

    let info = harness.service.start_pairing().unwrap();
    assert_eq!(info.replaces.as_deref(), Some("Phone"));
    let started = client.pair(&info.code, "Tablet").await.unwrap();
    let new = harness.allow_and_collect(&client, &started).await;

    assert!(
        stream.ends_within(Duration::from_secs(5)).await,
        "the old device's stream ends"
    );
    assert_eq!(
        client.get("/v1/hello", Some(&old)).await.unwrap().status,
        401
    );
    assert_eq!(
        client.get("/v1/hello", Some(&new)).await.unwrap().status,
        200
    );
}

#[tokio::test]
async fn refused_requests_are_summarised_in_the_activity_log_not_one_line_each() {
    let harness = Harness::new();
    let (_, client, _) = harness.paired().await;
    for _ in 0..50 {
        assert_eq!(
            client
                .get("/v1/hello", Some("fr1_wrong"))
                .await
                .unwrap()
                .status,
            401
        );
    }
    let summaries: Vec<_> = harness
        .activity()
        .into_iter()
        .filter(|line| line["event"] == "auth_failures")
        .collect();
    assert!(!summaries.is_empty(), "refusals are recorded");
    assert!(
        summaries.len() <= 2,
        "at most one summary a minute: {summaries:?}"
    );
}

#[tokio::test]
async fn revoking_rotates_the_certificate() {
    let harness = Harness::new();
    let (_, _, _) = harness.paired().await;
    let old_pin = harness.pin();
    let old_fingerprint = harness.service.status().fingerprint;
    harness.service.revoke().await.unwrap();

    let status = harness.service.status();
    let addr = status.listening.expect("still listening after a revoke");
    assert_ne!(status.fingerprint, old_fingerprint);
    let new_pin = harness.pin();
    assert_ne!(new_pin, old_pin, "a new certificate is on disk");
    assert!(
        TestClient::new(addr, old_pin).tls().await.is_err(),
        "the revoked device's pin no longer matches"
    );
    let fresh = TestClient::new(addr, new_pin);
    assert_eq!(fresh.get("/v1/hello", None).await.unwrap().status, 401);
    let info = harness.service.start_pairing().unwrap();
    assert!(info.link.ends_with(&new_pin.to_pin()));
}

#[tokio::test]
async fn revoking_while_off_rotates_the_certificate_for_next_time() {
    let harness = Harness::new();
    harness.paired().await;
    let old_pin = harness.pin();
    harness
        .service
        .configure(forge_remote::RemoteSettings {
            enabled: false,
            ..Harness::on()
        })
        .await
        .unwrap();
    harness.service.revoke().await.unwrap();
    assert_ne!(harness.pin(), old_pin);
    let (_, client) = harness.enable().await;
    assert_eq!(client.get("/v1/hello", None).await.unwrap().status, 401);
}
