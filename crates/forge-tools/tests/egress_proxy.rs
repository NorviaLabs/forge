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
async fn upstream() -> Option<(String, tokio::task::JoinHandle<()>)> {
    let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("skipping: this host denies binding a listener");
            return None;
        }
        Err(e) => panic!("bind upstream listener: {e}"),
    };
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let _ = sock.write_all(b"UPSTREAM-OK").await;
        }
    });
    Some((format!("127.0.0.1:{}", addr.port()), task))
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

/// Starts the loopback egress proxy, or skips the calling test on hosts that
/// deny binding a listener (CI sandboxes, agent harnesses).
async fn started_proxy(policy: EgressPolicy) -> Option<EgressProxy> {
    match EgressProxy::start(policy).await {
        Ok(proxy) => Some(proxy),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("skipping: this host denies binding a listener");
            None
        }
        Err(e) => panic!("egress proxy failed to start: {e}"),
    }
}

#[tokio::test]
async fn an_allowed_host_is_tunnelled_end_to_end() {
    let Some((addr, _up)) = upstream().await else {
        return;
    };
    let mut policy = EgressPolicy::new();
    policy.allow("127.0.0.1");
    let Some(proxy) = started_proxy(policy).await else {
        return;
    };

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
    let Some((addr, _up)) = upstream().await else {
        return;
    };
    let mut policy = EgressPolicy::new();
    policy.allow("crates.io");
    let Some(proxy) = started_proxy(policy).await else {
        return;
    };

    let status = connect_through(&proxy, &addr).await;
    assert!(
        status.contains("403"),
        "a host outside the allowlist must be refused, got: {status}"
    );
}

/// Fails closed: a proxy with no rules is not an open relay.
#[tokio::test]
async fn an_empty_policy_refuses_everything() {
    let Some((addr, _up)) = upstream().await else {
        return;
    };
    let Some(proxy) = started_proxy(EgressPolicy::new()).await else {
        return;
    };
    assert!(connect_through(&proxy, &addr).await.contains("403"));
}

/// The proxy speaks CONNECT only. A plain request must not be served, or it
/// becomes a general-purpose relay that ignores the allowlist entirely.
#[tokio::test]
async fn a_non_connect_request_is_rejected() {
    let Some(proxy) = started_proxy(EgressPolicy::new()).await else {
        return;
    };
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
    let Some(proxy) = started_proxy(EgressPolicy::new()).await else {
        return;
    };
    let status = connect_through(&proxy, "definitely-not-allowed.invalid:443").await;
    assert!(status.contains("403"), "got: {status}");
}

// ---------------------------------------------------------------------------
// The Unix-socket bridge.
//
// A sandbox with --unshare-net has no loopback, so a TCP proxy is unreachable
// from inside it. A Unix socket is a filesystem object rather than a network
// one, so a bind-mounted socket still crosses that boundary — which is how a
// confined process reaches exactly one destination without a userspace network
// stack. The filtering must be identical on both transports, or the bridge
// becomes the way around the allowlist.
// ---------------------------------------------------------------------------

use tokio::net::UnixStream;

async fn connect_over_uds(path: &std::path::Path, target: &str) -> String {
    let mut stream = UnixStream::connect(path).await.unwrap();
    stream
        .write_all(format!("CONNECT {target} HTTP/1.1\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).await.unwrap();
    status
}

#[tokio::test]
async fn the_unix_bridge_tunnels_an_allowed_host() {
    let Some((addr, _up)) = upstream().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("egress.sock");

    let mut policy = EgressPolicy::new();
    policy.allow("127.0.0.1");
    let Some(mut proxy) = started_proxy(policy.clone()).await else {
        return;
    };
    proxy.serve_on_unix_socket(&sock, policy).await.unwrap();
    assert_eq!(proxy.socket_path(), Some(sock.as_path()));

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    stream
        .write_all(format!("CONNECT {addr} HTTP/1.1\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).await.unwrap();
    assert!(status.contains("200"), "expected a tunnel, got: {status}");

    let mut blank = String::new();
    reader.read_line(&mut blank).await.unwrap();
    let mut body = vec![0u8; 11];
    tokio::time::timeout(Duration::from_secs(5), reader.read_exact(&mut body))
        .await
        .expect("the bridge must carry bytes")
        .unwrap();
    assert_eq!(&body, b"UPSTREAM-OK");
}

/// The bridge must not be a way around the allowlist.
#[tokio::test]
async fn the_unix_bridge_enforces_the_same_allowlist() {
    let Some((addr, _up)) = upstream().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("egress.sock");

    let mut policy = EgressPolicy::new();
    policy.allow("crates.io");
    let Some(mut proxy) = started_proxy(policy.clone()).await else {
        return;
    };
    proxy.serve_on_unix_socket(&sock, policy).await.unwrap();

    assert!(
        connect_over_uds(&sock, &addr).await.contains("403"),
        "a denied host must be denied on both transports"
    );
}

/// A stale socket file would make the next bind fail, so binding must replace
/// one left behind by an earlier run.
#[tokio::test]
async fn a_stale_socket_file_does_not_block_binding() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("egress.sock");
    std::fs::write(&sock, b"stale").unwrap();

    let Some(mut proxy) = started_proxy(EgressPolicy::new()).await else {
        return;
    };
    proxy
        .serve_on_unix_socket(&sock, EgressPolicy::new())
        .await
        .expect("a stale path must be replaced, not fatal");
}
