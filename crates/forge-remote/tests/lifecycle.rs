//! Invariant: with the service off, nothing listens. Turning it on listens
//! on pinned TLS only; turning it off closes everything at once.

mod common;

use std::time::Duration;

use common::{eventually, Harness};
use forge_remote::testing::{plain_http, refuses_connections, TestClient};
use forge_remote::tls::Fingerprint;
use forge_remote::{RemoteError, RemoteSettings};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn nothing_listens_by_default() {
    let harness = Harness::new();
    let status = harness.service.start_if_enabled().await;
    assert!(!status.enabled);
    assert_eq!(status.listening, None);
    assert_eq!(status.error, None);
    assert!(
        harness.backend.interest().is_none(),
        "nothing even subscribes to sessions while off"
    );
}

#[tokio::test]
async fn a_configured_port_stays_closed_while_the_service_is_off() {
    let harness = Harness::new();
    let port = free_port();
    let status = harness
        .service
        .configure(RemoteSettings {
            enabled: false,
            port,
            ..RemoteSettings::default()
        })
        .await
        .unwrap();
    assert_eq!(status.listening, None);
    harness.service.start_if_enabled().await;
    assert!(refuses_connections(([127, 0, 0, 1], port).into()).await);
}

#[tokio::test]
async fn turning_it_on_listens_on_loopback_with_pinned_tls_only() {
    let harness = Harness::new();
    let (addr, client) = harness.enable().await;
    assert!(addr.ip().is_loopback());
    let status = harness.service.status();
    assert!(status.enabled);
    assert_eq!(status.listening, Some(addr));
    assert_eq!(
        status.fingerprint.as_deref(),
        Some(harness.pin().display_groups().as_str())
    );

    let reply = client.get("/v1/hello", None).await.unwrap();
    assert_eq!(reply.status, 401, "HTTPS works and wants a token");

    let plain = plain_http(addr).await.unwrap();
    assert!(
        !String::from_utf8_lossy(&plain).starts_with("HTTP/"),
        "plain HTTP gets no HTTP answer"
    );

    let impostor_pin = Fingerprint::of_der(b"some other certificate");
    let wrong = TestClient::new(addr, impostor_pin);
    assert!(
        wrong.tls().await.is_err(),
        "a client pinned elsewhere fails the handshake"
    );
}

#[tokio::test]
async fn turning_it_off_closes_the_port_and_every_stream() {
    let harness = Harness::new();
    let (addr, client, token) = harness.paired().await;
    let session_id = forge_types::SessionId::new_v4();
    harness
        .backend
        .put(forge_remote::testing::session(session_id, "s"));
    let mut stream = client
        .events(session_id, Some(&token), None)
        .await
        .unwrap_or_else(|r| panic!("{} {}", r.status, r.text));
    stream.next().await.expect("the first snapshot");

    let status = harness
        .service
        .configure(RemoteSettings {
            enabled: false,
            ..Harness::on()
        })
        .await
        .unwrap();
    assert!(!status.enabled);
    assert_eq!(status.listening, None);
    assert!(
        stream.ends_within(Duration::from_secs(5)).await,
        "open streams end"
    );
    assert!(refuses_connections(addr).await, "nothing listens");
    std::net::TcpListener::bind(addr).expect("the port is released at once");
    assert!(harness
        .activity()
        .iter()
        .any(|line| line["event"] == "disabled"));
}

#[tokio::test]
async fn turning_it_off_withdraws_the_pairing_code() {
    let harness = Harness::new();
    harness.enable().await;
    let info = harness.service.start_pairing().unwrap();
    harness
        .service
        .configure(RemoteSettings {
            enabled: false,
            ..Harness::on()
        })
        .await
        .unwrap();
    assert_eq!(harness.service.status().pairing_expires_at, None);
    let (_, client) = harness.enable().await;
    let refused = client.pair(&info.code, "Phone").await.unwrap_err();
    assert_eq!(refused.status, 401);
}

#[tokio::test]
async fn a_code_can_only_be_made_while_listening() {
    let harness = Harness::new();
    assert!(matches!(
        harness.service.start_pairing(),
        Err(RemoteError::NotListening)
    ));
}

#[tokio::test]
async fn a_port_in_use_is_reported_in_the_status() {
    let harness = Harness::new();
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let status = harness
        .service
        .configure(RemoteSettings {
            port,
            ..Harness::on()
        })
        .await
        .expect("a busy port is a status, not an error");
    assert!(status.enabled);
    assert_eq!(status.listening, None);
    assert!(
        status
            .error
            .as_deref()
            .is_some_and(|e| e.contains(&port.to_string())),
        "{status:?}"
    );
}

#[tokio::test]
async fn it_listens_again_at_launch_when_it_was_left_on() {
    let harness = Harness::new();
    let (_, _, token) = harness.paired().await;
    let pin = harness.pin();
    harness.service.shutdown().await;
    assert!(
        harness.service.settings().enabled,
        "quitting keeps the setting"
    );
    assert_eq!(harness.service.status().listening, None);

    let again = forge_remote::RemoteService::new(forge_remote::ServiceOptions {
        dir: harness.dir(),
        backend: harness.backend.clone(),
        clock: harness.clock.clone(),
        limits: forge_remote::Limits::default(),
        hub: forge_remote::hub::HubConfig::default(),
    });
    let status = again.start_if_enabled().await;
    let addr = status.listening.expect("listening after a restart");
    assert_eq!(
        harness.pin(),
        pin,
        "the certificate is kept, so the pin holds"
    );
    let client = TestClient::new(addr, pin);
    let hello = client.get("/v1/hello", Some(&token)).await.unwrap();
    assert_eq!(hello.status, 200, "the paired device is remembered");
    again.shutdown().await;
}

#[tokio::test]
async fn the_status_follows_pairing_and_connections() {
    let harness = Harness::new();
    let (_, client) = harness.enable().await;
    let mut status = harness.service.subscribe_status();
    let info = harness.service.start_pairing().unwrap();
    assert!(harness.service.status().pairing_expires_at.is_some());
    let started = client.pair(&info.code, "Priya's iPhone").await.unwrap();
    let token = harness.allow_and_collect(&client, &started).await;
    status.changed().await.unwrap();
    let now = harness.service.status();
    assert_eq!(now.device.map(|d| d.name), Some("Priya's iPhone".into()));
    assert_eq!(now.pairing_expires_at, None, "the code is used up");
    assert!(!now.connected);

    let session_id = forge_types::SessionId::new_v4();
    harness
        .backend
        .put(forge_remote::testing::session(session_id, "s"));
    let stream = client.events(session_id, Some(&token), None).await.unwrap();
    assert!(eventually(|| harness.service.status().connected).await);
    drop(stream);
    assert!(eventually(|| !harness.service.status().connected).await);
}
