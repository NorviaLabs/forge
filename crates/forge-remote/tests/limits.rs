//! What one request or connection may cost.

mod common;

use std::time::Duration;

use common::Harness;
use forge_remote::hub::HubConfig;
use forge_remote::Limits;
use serde_json::json;
use tokio::io::AsyncReadExt;

#[tokio::test]
async fn an_oversized_body_is_refused() {
    let harness = Harness::new();
    let (_, client) = harness.enable().await;
    let info = harness.service.start_pairing().unwrap();
    let big = json!({"code": info.code, "device_name": "x".repeat(64 * 1024)});
    let reply = client.post("/v1/pair", None, big).await.unwrap();
    assert_eq!(reply.status, 413);
    assert!(
        client.pair(&info.code, "Phone").await.is_ok(),
        "the code survives"
    );
}

#[tokio::test]
async fn a_connection_that_never_sends_a_request_is_closed() {
    let harness = Harness::with(
        Limits {
            header_timeout: Duration::from_millis(300),
            ..Limits::default()
        },
        HubConfig::default(),
    );
    let (_, client) = harness.enable().await;
    let mut idle = client.tls().await.expect("the handshake completes");
    let mut buf = [0u8; 64];
    let read = tokio::time::timeout(Duration::from_secs(3), idle.read(&mut buf)).await;
    assert!(
        matches!(read, Ok(Ok(0)) | Ok(Err(_))),
        "the server hangs up on a silent client: {read:?}"
    );
}

#[tokio::test]
async fn connections_beyond_the_cap_are_turned_away() {
    let harness = Harness::with(
        Limits {
            max_connections: 2,
            header_timeout: Duration::from_secs(30),
            ..Limits::default()
        },
        HubConfig::default(),
    );
    let (_, client) = harness.enable().await;
    let first = client.tls().await.unwrap();
    let second = client.tls().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let third = client.get("/v1/hello", None).await;
    assert!(
        third.is_err(),
        "a third connection gets no answer: {:?}",
        third.map(|r| r.status)
    );
    drop((first, second));
    let mut answered = false;
    for _ in 0..100 {
        if let Ok(reply) = client.get("/v1/hello", None).await {
            assert_eq!(reply.status, 401);
            answered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(answered, "freed places are reused");
}

#[tokio::test]
async fn a_body_that_never_finishes_is_cut_off() {
    use tokio::io::AsyncWriteExt;
    let harness = Harness::with(
        Limits {
            body_timeout: Duration::from_millis(300),
            ..Limits::default()
        },
        HubConfig::default(),
    );
    let (_, client) = harness.enable().await;
    let mut slow = client.tls().await.unwrap();
    slow.write_all(
        b"POST /v1/pair HTTP/1.1\r\nHost: forge-remote\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"code\"",
    )
    .await
    .unwrap();
    slow.flush().await.unwrap();
    let mut answer = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(3), slow.read_to_end(&mut answer)).await;
    assert!(
        read.is_ok(),
        "the server must not wait forever for the rest of the body"
    );
    let text = String::from_utf8_lossy(&answer);
    assert!(
        text.is_empty() || text.starts_with("HTTP/1.1 408"),
        "a slow body is refused or dropped: {text}"
    );
}

#[tokio::test]
async fn one_address_cannot_take_every_connection() {
    let harness = Harness::with(
        Limits {
            max_connections: 16,
            max_connections_per_peer: 2,
            header_timeout: Duration::from_secs(30),
            ..Limits::default()
        },
        HubConfig::default(),
    );
    let (_, client) = harness.enable().await;
    let first = client.tls().await.unwrap();
    let second = client.tls().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        client.get("/v1/hello", None).await.is_err(),
        "a third connection from the same address is turned away"
    );
    drop((first, second));
    let mut answered = false;
    for _ in 0..100 {
        if client.get("/v1/hello", None).await.is_ok() {
            answered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(answered, "the address's places are freed");
}
