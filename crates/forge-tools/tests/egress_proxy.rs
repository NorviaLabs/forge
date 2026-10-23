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

/// Plain HTTP is forwarded only through the allowlist: with no rules, an
/// absolute-form request is refused exactly like a CONNECT would be, so the
/// forwarding path is not a general-purpose relay.
#[tokio::test]
async fn a_plain_http_request_obeys_the_allowlist() {
    let Some(proxy) = started_proxy(EgressPolicy::new()).await else {
        return;
    };
    let status = request_status(
        &proxy,
        "GET http://crates.io/ HTTP/1.1\r\nHost: crates.io\r\n\r\n",
    )
    .await;
    assert_eq!(status, "HTTP/1.1 403 Forge Sandbox Denied\r\n");
}

/// Requests that are neither CONNECT nor absolute-form plain HTTP are 400s.
#[tokio::test]
async fn an_unsupported_request_is_rejected() {
    let mut policy = EgressPolicy::new();
    policy.allow("*");
    let Some(proxy) = started_proxy(policy).await else {
        return;
    };
    for request in [
        "GET /secrets HTTP/1.1\r\nHost: crates.io\r\n\r\n",
        "GET https://crates.io/ HTTP/1.1\r\n\r\n",
        "GET http://user@crates.io/ HTTP/1.1\r\n\r\n",
        "GET http://crates.io/ HTTP/1.1\r\n folded\r\n\r\n",
        "garbage\r\n\r\n",
        "\r\n\r\n",
    ] {
        let status = request_status(&proxy, request).await;
        assert!(status.contains("400"), "{request:?} got {status:?}");
    }
}

async fn request_status(proxy: &EgressProxy, request: &str) -> String {
    let mut stream = TcpStream::connect(proxy.addr()).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).await.unwrap();
    status
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

// ---------------------------------------------------------------------------
// The gateway: address checks, pinning, idle timeout, attribution.
// ---------------------------------------------------------------------------

use forge_tools::egress::{
    EgressDenial, EgressInvocation, EgressOptions, EgressOutcome, PRIVATE_ADDRESS_DENIED_REASON,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

async fn started_proxy_with(policy: EgressPolicy, options: EgressOptions) -> Option<EgressProxy> {
    match EgressProxy::start_with_options(policy, options).await {
        Ok(proxy) => Some(proxy),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("skipping: this host denies binding a listener");
            None
        }
        Err(e) => panic!("egress proxy failed to start: {e}"),
    }
}

/// An upstream that greets, then reads to end of stream before closing.
/// Counts accepted connections.
async fn counting_upstream() -> Option<(u16, Arc<AtomicUsize>, tokio::task::JoinHandle<()>)> {
    let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return None,
        Err(e) => panic!("bind upstream listener: {e}"),
    };
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&accepted);
    let task = tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            seen.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let _ = sock.write_all(b"UPSTREAM-OK").await;
                let mut sink = Vec::new();
                let _ = sock.read_to_end(&mut sink).await;
            });
        }
    });
    Some((port, accepted, task))
}

async fn wait_for_events(
    proxy: &EgressProxy,
    want: usize,
) -> Vec<forge_tools::egress::EgressEvent> {
    let mut events = Vec::new();
    for _ in 0..100 {
        events.extend(proxy.shared().drain_events());
        if events.len() >= want {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    events
}

/// A broad grant must not reach loopback: `host(*)` is about public names,
/// and a name that resolves to 127.0.0.1 would otherwise be a way onto the
/// host's own services. No upstream socket may be opened.
#[tokio::test]
async fn a_wildcard_grant_does_not_reach_a_private_address() {
    let Some((port, accepted, _up)) = counting_upstream().await else {
        return;
    };
    let mut policy = EgressPolicy::new();
    policy.allow("*");
    let Some(proxy) = started_proxy(policy).await else {
        return;
    };
    for target in [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
        "169.254.169.254:80".to_string(),
        "10.0.0.1:443".to_string(),
    ] {
        let status = connect_through(&proxy, &target).await;
        assert_eq!(
            status,
            format!("HTTP/1.1 403 {PRIVATE_ADDRESS_DENIED_REASON}\r\n"),
            "{target} must be refused for its address"
        );
    }
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        0,
        "no upstream dial may happen"
    );
    assert_eq!(
        proxy.shared().take_denied_host(),
        None,
        "an address refusal is not a host a grant could fix"
    );
    let events = wait_for_events(&proxy, 5).await;
    assert_eq!(events.len(), 5);
    assert!(events
        .iter()
        .all(|e| e.outcome == EgressOutcome::Denied(EgressDenial::PrivateAddress)));
}

/// Dialling uses the address that was checked: the event names the resolved
/// address the tunnel actually went to, and it is one the name resolved to.
#[tokio::test]
async fn the_dialled_address_is_the_checked_one() {
    let Some((port, accepted, _up)) = counting_upstream().await else {
        return;
    };
    let mut policy = EgressPolicy::new();
    policy.allow("*");
    let options = EgressOptions {
        allow_private_destinations: true,
        ..EgressOptions::default()
    };
    let Some(proxy) = started_proxy_with(policy, options).await else {
        return;
    };
    let mut stream = TcpStream::connect(proxy.tcp_addr()).await.unwrap();
    stream
        .write_all(format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).await.unwrap();
    assert!(status.contains("200"), "{status}");
    drop(reader);

    let events = wait_for_events(&proxy, 1).await;
    let upstream = events[0]
        .upstream
        .expect("an allowed tunnel names its upstream");
    let resolved: Vec<_> = tokio::net::lookup_host(("localhost", port))
        .await
        .unwrap()
        .collect();
    assert!(
        resolved.contains(&upstream),
        "{upstream} not in {resolved:?}"
    );
    assert_eq!(upstream.port(), port);
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}

/// A tunnel that carries nothing is closed on both sides after the idle
/// timeout, instead of pinning sockets for the life of the session.
#[tokio::test]
async fn an_idle_tunnel_is_closed() {
    let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return,
        Err(e) => panic!("{e}"),
    };
    let port = listener.local_addr().unwrap().port();
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        // Silent upstream: reads until the gateway closes it.
        let mut sink = Vec::new();
        let _ = sock.read_to_end(&mut sink).await;
        let _ = closed_tx.send(());
    });

    let mut policy = EgressPolicy::new();
    policy.allow("127.0.0.1");
    let options = EgressOptions {
        idle_timeout: Duration::from_millis(200),
        ..EgressOptions::default()
    };
    let Some(proxy) = started_proxy_with(policy, options).await else {
        return;
    };
    let mut stream = TcpStream::connect(proxy.addr()).await.unwrap();
    stream
        .write_all(format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).await.unwrap();
    assert!(status.contains("200"), "{status}");
    let mut blank = String::new();
    reader.read_line(&mut blank).await.unwrap();

    let mut rest = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), reader.read_to_end(&mut rest))
        .await
        .expect("an idle tunnel must be closed on the client side");
    assert_eq!(read.unwrap(), 0);
    tokio::time::timeout(Duration::from_secs(5), closed_rx)
        .await
        .expect("an idle tunnel must be closed on the upstream side")
        .unwrap();
}

/// Each CONNECT through an invocation endpoint is recorded with the
/// invocation's label and the bytes carried each way, in the session-wide
/// log rather than the invocation's own denial queue.
#[tokio::test]
async fn connections_are_recorded_with_label_and_byte_counts() {
    let Some((port, _accepted, _up)) = counting_upstream().await else {
        return;
    };
    let mut policy = EgressPolicy::new();
    policy.allow("127.0.0.1");
    let Some(proxy) = started_proxy(policy).await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let template = forge_tools::sandbox::EgressGrant {
        proxy_port: proxy.addr().port(),
        socket_path: dir.path().join("session.sock"),
        control: Some(proxy.shared().clone()),
    };
    let invocation = EgressInvocation::start_labeled(Some(&template), Some("call-123"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(invocation.label(), Some("call-123"));
    let invocation_port = invocation.tcp_addr().port();

    let mut stream = TcpStream::connect(("127.0.0.1", invocation_port))
        .await
        .unwrap();
    stream
        .write_all(format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\nping").as_bytes())
        .await
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).await.unwrap();
    assert!(status.contains("200"), "{status}");
    let mut blank = String::new();
    reader.read_line(&mut blank).await.unwrap();
    let mut greeting = [0u8; 11];
    reader.read_exact(&mut greeting).await.unwrap();
    reader.get_mut().shutdown().await.unwrap();
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_to_end(&mut rest))
        .await
        .unwrap()
        .unwrap();

    let denied = connect_status_on(invocation_port, "blocked.example:443").await;
    assert!(denied.contains("403"), "{denied}");

    let events = wait_for_events(&proxy, 2).await;
    let allowed = events.iter().find(|e| e.allowed()).expect("tunnel event");
    assert_eq!(allowed.label.as_deref(), Some("call-123"));
    assert_eq!(allowed.host, "127.0.0.1");
    assert_eq!(allowed.port, port);
    assert_eq!(
        allowed.bytes_up, 4,
        "the pipelined `ping` counts as upstream bytes"
    );
    assert_eq!(allowed.bytes_down, 11);
    let refused = events.iter().find(|e| !e.allowed()).expect("denial event");
    assert_eq!(refused.label.as_deref(), Some("call-123"));
    assert_eq!(refused.host, "blocked.example");
    assert_eq!(refused.outcome, EgressOutcome::Denied(EgressDenial::Policy));
}

async fn connect_status_on(port: u16, target: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream
        .write_all(format!("CONNECT {target} HTTP/1.1\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).await.unwrap();
    status
}

/// The loopback TCP listener is a first-class gateway entry point.
#[tokio::test]
async fn the_tcp_listener_is_loopback_and_tunnels() {
    let Some((addr, _up)) = upstream().await else {
        return;
    };
    let mut policy = EgressPolicy::new();
    policy.allow("127.0.0.1");
    let Some(proxy) = started_proxy(policy).await else {
        return;
    };
    let tcp = proxy.tcp_addr();
    assert!(tcp.ip().is_loopback(), "{tcp}");
    assert_ne!(tcp.port(), 0);
    let mut stream = TcpStream::connect(tcp).await.unwrap();
    stream
        .write_all(format!("CONNECT {addr} HTTP/1.1\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).await.unwrap();
    assert!(status.contains("200"), "{status}");
}

// ---------------------------------------------------------------------------
// Plain-HTTP forwarding and the per-call gateway.
// ---------------------------------------------------------------------------

use forge_tools::egress::CallGateway;

/// A one-shot HTTP origin: reads one request (head plus a Content-Length
/// body), hands the raw request to the test, answers with the body echoed and
/// a keep-alive response the gateway must rewrite, then closes.
async fn http_origin() -> Option<(u16, tokio::sync::mpsc::UnboundedReceiver<String>)> {
    let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return None,
        Err(e) => panic!("bind origin: {e}"),
    };
    let port = listener.local_addr().unwrap().port();
    let (seen_tx, seen_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let seen = seen_tx.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(sock);
                let mut head = String::new();
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                    head.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0u8; length];
                reader.read_exact(&mut body).await.unwrap();
                let body = String::from_utf8(body).unwrap();
                let _ = seen.send(format!("{head}{body}"));
                let reply = format!("echo:{body}");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\
                     Keep-Alive: timeout=5\r\nX-Origin: yes\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = reader.get_mut().write_all(response.as_bytes()).await;
            });
        }
    });
    Some((port, seen_rx))
}

/// Send `request` and read the whole response until the gateway closes.
async fn exchange(addr: std::net::SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_string(&mut response))
        .await
        .expect("the gateway must close after one exchange")
        .unwrap();
    response
}

/// An allowed absolute-form request is rewritten to origin-form, stripped of
/// hop-by-hop and proxy headers, sent with `Connection: close`, and the
/// response streams back with `Connection: close`. The exchange is logged
/// like a tunnel, with its bytes counted.
#[tokio::test]
async fn an_allowed_plain_http_request_is_forwarded() {
    let Some((port, mut seen)) = http_origin().await else {
        return;
    };
    let mut policy = EgressPolicy::new();
    policy.allow("*");
    let options = EgressOptions {
        allow_private_destinations: true,
        ..EgressOptions::default()
    };
    let Some(proxy) = started_proxy_with(policy, options).await else {
        return;
    };
    let response = exchange(
        proxy.tcp_addr(),
        &format!(
            "POST http://localhost:{port}/echo?x=1 HTTP/1.1\r\nHost: elsewhere.example\r\n\
             Proxy-Authorization: Basic c2VjcmV0\r\nProxy-Connection: keep-alive\r\n\
             Connection: X-Hop\r\nX-Hop: 1\r\nX-Kept: yes\r\nContent-Length: 4\r\n\r\nping"
        ),
    )
    .await;

    let upstream = seen.recv().await.unwrap();
    assert!(
        upstream.starts_with("POST /echo?x=1 HTTP/1.1\r\n"),
        "{upstream}"
    );
    assert!(
        upstream.contains(&format!("Host: localhost:{port}\r\n")),
        "{upstream}"
    );
    assert!(upstream.contains("Connection: close\r\n"), "{upstream}");
    assert!(upstream.contains("X-Kept: yes\r\n"), "{upstream}");
    assert!(upstream.ends_with("\r\n\r\nping"), "{upstream}");
    for stripped in ["elsewhere.example", "Proxy-", "X-Hop"] {
        assert!(
            !upstream.contains(stripped),
            "{stripped} leaked: {upstream}"
        );
    }

    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
    assert!(response.contains("Connection: close\r\n"), "{response}");
    assert!(response.contains("X-Origin: yes\r\n"), "{response}");
    assert!(!response.contains("keep-alive"), "{response}");
    assert!(!response.contains("Keep-Alive"), "{response}");
    assert!(response.ends_with("\r\n\r\necho:ping"), "{response}");

    let events = wait_for_events(&proxy, 1).await;
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert!(event.allowed(), "{event:?}");
    assert_eq!((event.host.as_str(), event.port), ("localhost", port));
    assert_eq!(event.upstream.unwrap().port(), port);
    assert!(event.bytes_up > 4, "{event:?}");
    assert_eq!(event.bytes_down as usize, response.len());
}

/// A plain-HTTP request to a host outside the allowlist is refused with the
/// same 403 and reason as CONNECT, queues the host, and is logged.
#[tokio::test]
async fn a_denied_plain_http_request_is_refused_and_logged() {
    let Some((port, _seen)) = http_origin().await else {
        return;
    };
    let mut policy = EgressPolicy::new();
    policy.allow("crates.io");
    let Some(proxy) = started_proxy(policy).await else {
        return;
    };
    let response = exchange(
        proxy.addr(),
        &format!("GET http://blocked.example:{port}/ HTTP/1.1\r\n\r\n"),
    )
    .await;
    assert!(
        response.starts_with("HTTP/1.1 403 Forge Sandbox Denied\r\n"),
        "{response}"
    );
    assert!(
        response.ends_with("Forge Sandbox Denied: host blocked.example is not allowed\n"),
        "{response}"
    );
    assert_eq!(
        proxy.shared().take_denied_host().as_deref(),
        Some("blocked.example")
    );
    let events = wait_for_events(&proxy, 1).await;
    assert_eq!(events[0].host, "blocked.example");
    assert_eq!(
        events[0].outcome,
        EgressOutcome::Denied(EgressDenial::Policy)
    );
}

/// A permitted name resolving to a private address is refused for its
/// address, on the forwarding path too, without dialling.
#[tokio::test]
async fn a_plain_http_request_to_a_private_address_is_refused() {
    let Some((port, accepted, _up)) = counting_upstream().await else {
        return;
    };
    let mut policy = EgressPolicy::new();
    policy.allow("*");
    let Some(proxy) = started_proxy(policy).await else {
        return;
    };
    let response = exchange(
        proxy.addr(),
        &format!("GET http://localhost:{port}/ HTTP/1.1\r\n\r\n"),
    )
    .await;
    assert!(
        response.starts_with(&format!("HTTP/1.1 403 {PRIVATE_ADDRESS_DENIED_REASON}\r\n")),
        "{response}"
    );
    assert_eq!(accepted.load(Ordering::SeqCst), 0, "no upstream dial");
    assert_eq!(proxy.shared().take_denied_host(), None);
    let events = wait_for_events(&proxy, 1).await;
    assert_eq!(
        events[0].outcome,
        EgressOutcome::Denied(EgressDenial::PrivateAddress)
    );
}

fn template_for(proxy: &EgressProxy, dir: &tempfile::TempDir) -> forge_tools::sandbox::EgressGrant {
    forge_tools::sandbox::EgressGrant {
        proxy_port: proxy.addr().port(),
        socket_path: dir.path().join("session.sock"),
        control: Some(proxy.shared().clone()),
    }
}

/// The per-call gateway: its own loopback port, the session's live policy,
/// connections labelled with the call id, refusals in its own queue only, and
/// nothing listening once it is dropped.
#[tokio::test]
async fn a_call_gateway_is_labelled_isolated_and_stops_on_drop() {
    let Some((port, _seen)) = http_origin().await else {
        return;
    };
    let mut policy = EgressPolicy::new();
    policy.allow("127.0.0.1");
    let Some(proxy) = started_proxy(policy).await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let template = template_for(&proxy, &dir);
    let ctx = forge_tools::ToolContext {
        egress: Some(Arc::new(template.clone())),
        ..forge_tools::ToolContext::new(dir.path().to_path_buf())
    }
    .with_tool_call_id("call-7");
    let gateway = ctx.gateway_for_call().await.unwrap().expect("a live grant");
    let other = CallGateway::start(Some(&template), Some("call-8"))
        .await
        .unwrap()
        .unwrap();

    let addr = gateway.addr();
    assert!(addr.ip().is_loopback(), "{addr}");
    assert_ne!(addr.port(), proxy.addr().port());
    assert_eq!(
        gateway.proxy_url(),
        format!("http://127.0.0.1:{}", addr.port())
    );
    assert_eq!(gateway.label(), Some("call-7"));

    let ok = exchange(
        addr,
        &format!("GET http://127.0.0.1:{port}/ HTTP/1.1\r\n\r\n"),
    )
    .await;
    assert!(ok.starts_with("HTTP/1.1 200 OK"), "{ok}");
    let denied = connect_status_on(addr.port(), "blocked.example:443").await;
    assert!(denied.contains("403"), "{denied}");

    assert_eq!(gateway.denied_hosts(), vec!["blocked.example".to_string()]);
    assert!(other.denied_hosts().is_empty(), "queues are per call");
    assert_eq!(proxy.shared().take_denied_host(), None, "not the session's");
    let error = gateway
        .sandbox_denied("HTTP 403 from the gateway")
        .expect("a refused host becomes a sandbox denial");
    let forge_tools::ToolError::SandboxDenied {
        denied_host,
        content,
        ..
    } = error
    else {
        panic!("expected SandboxDenied");
    };
    assert_eq!(denied_host.as_deref(), Some("blocked.example"));
    assert!(
        content.starts_with("HTTP 403 from the gateway\n"),
        "{content}"
    );
    assert!(gateway.sandbox_denied("again").is_none(), "consumed");

    let events = wait_for_events(&proxy, 2).await;
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(events.iter().all(|e| e.label.as_deref() == Some("call-7")));

    // A session grant reaches a call that already started.
    proxy.shared().grant_host("**.example");
    assert!(gateway.permits_host("blocked.example"));

    drop(gateway);
    let mut refused = false;
    for _ in 0..50 {
        if TcpStream::connect(addr).await.is_err() {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(refused, "a dropped gateway must stop listening on {addr}");
}
