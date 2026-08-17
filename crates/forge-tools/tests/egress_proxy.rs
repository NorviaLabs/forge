//! The proxy actually filtering, over real sockets.
//!
//! Hermetic on purpose: the "allowed" destination is a listener this test
//! owns, so nothing here depends on the internet being reachable or on a
//! third party staying up. A network test that needs the network is a flaky
//! test.

use std::time::Duration;

use forge_tools::egress::{EgressPolicy, EgressProxy};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// A stand-in upstream that greets whoever connects.
async fn upstream() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let _ = sock.write_all(b"UPSTREAM-OK").await;
        }
    });
    (format!("127.0.0.1:{}", addr.port()), task)
}

async fn connect_through(proxy: &EgressProxy, target: &str) -> String {
    let mut stream = TcpStream::connect(proxy.addr()).await.unwrap();
    stream
        .write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).await.unwrap();
    status
}

#[tokio::test]
async fn an_allowed_host_is_tunnelled_end_to_end() {
    let (addr, _up) = upstream().await;
    let mut policy = EgressPolicy::new();
    policy.allow("127.0.0.1");
    let proxy = EgressProxy::start(policy).await.unwrap();

    let mut stream = TcpStream::connect(proxy.addr()).await.unwrap();
    stream
        .write_all(format!("CONNECT {addr} HTTP/1.1\r\n\r\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).await.unwrap();
    assert!(status.contains("200"), "expected a tunnel, got: {status}");

    // Drain the blank line, then read what upstream sent through the tunnel.
    let mut blank = String::new();
    reader.read_line(&mut blank).await.unwrap();
    let mut body = vec![0u8; 11];
    tokio::time::timeout(Duration::from_secs(5), reader.read_exact(&mut body))
        .await
        .expect("tunnel should carry bytes")
        .unwrap();
    assert_eq!(&body, b"UPSTREAM-OK", "bytes must pass through untouched");
}

#[tokio::test]
async fn a_host_outside_the_allowlist_is_refused() {
    let (addr, _up) = upstream().await;
    let mut policy = EgressPolicy::new();
    policy.allow("crates.io");
    let proxy = EgressProxy::start(policy).await.unwrap();

    let status = connect_through(&proxy, &addr).await;
    assert!(
        status.contains("403"),
        "a host outside the allowlist must be refused, got: {status}"
    );
}

/// Fails closed: a proxy with no rules is not an open relay.
#[tokio::test]
async fn an_empty_policy_refuses_everything() {
    let (addr, _up) = upstream().await;
    let proxy = EgressProxy::start(EgressPolicy::new()).await.unwrap();
    assert!(connect_through(&proxy, &addr).await.contains("403"));
}

/// The proxy speaks CONNECT only. A plain request must not be served, or it
/// becomes a general-purpose relay that ignores the allowlist entirely.
#[tokio::test]
async fn a_non_connect_request_is_rejected() {
    let proxy = EgressProxy::start(EgressPolicy::with_default_ecosystems())
        .await
        .unwrap();
    let mut stream = TcpStream::connect(proxy.addr()).await.unwrap();
    stream
        .write_all(b"GET http://crates.io/ HTTP/1.1\r\nHost: crates.io\r\n\r\n")
        .await
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).await.unwrap();
    assert!(
        status.contains("400"),
        "only CONNECT is served, got: {status}"
    );
}

/// A denied host must be refused before any socket is opened to it — the
/// decision cannot depend on the destination being reachable.
#[tokio::test]
async fn refusal_does_not_require_the_destination_to_exist() {
    let proxy = EgressProxy::start(EgressPolicy::with_default_ecosystems())
        .await
        .unwrap();
    let status = connect_through(&proxy, "definitely-not-allowed.invalid:443").await;
    assert!(status.contains("403"), "got: {status}");
}
