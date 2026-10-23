//! `web_fetch` ignores the user's proxy environment when a gateway exists.
//!
//! Its own test binary, holding a single test, because it sets process-wide
//! proxy variables: nothing else may run in this process while they are set.

use std::sync::Arc;
use std::time::Duration;

use forge_tools::egress::{EgressOptions, EgressPolicy, EgressProxy};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// An origin that answers every request with a fixed body.
async fn origin() -> Option<u16> {
    let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return None,
        Err(e) => panic!("bind origin: {e}"),
    };
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0_u8; 4096];
                let mut head = Vec::new();
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nfrom origin",
                    )
                    .await;
            });
        }
    });
    Some(port)
}

#[tokio::test]
async fn system_proxy_variables_cannot_route_around_the_gateway() {
    let Some(port) = origin().await else {
        eprintln!("skipping: this host denies binding a listener");
        return;
    };
    let mut policy = EgressPolicy::new();
    policy.allow("localhost");
    let proxy = match EgressProxy::start_with_options(
        policy,
        EgressOptions {
            allow_private_destinations: true,
            ..Default::default()
        },
    )
    .await
    {
        Ok(proxy) => proxy,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return,
        Err(e) => panic!("egress proxy failed to start: {e}"),
    };

    // Honouring any of these would send the request to a dead port, and
    // honouring NO_PROXY would send it direct, skipping the gateway log.
    for var in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        std::env::set_var(var, "http://127.0.0.1:1");
    }
    std::env::set_var("NO_PROXY", "*");
    std::env::set_var("no_proxy", "*");

    let dir = std::env::temp_dir();
    let ctx = forge_tools::ToolContext {
        egress: Some(Arc::new(forge_tools::sandbox::EgressGrant {
            proxy_port: proxy.addr().port(),
            socket_path: dir.join("forge-web-fetch-gateway-test.sock"),
            control: Some(proxy.shared().clone()),
        })),
        ..forge_tools::ToolContext::new(dir.clone())
    }
    .with_tool_call_id("call-env");
    let out = forge_tools::web_fetch_tool()
        .call(&ctx, json!({"url": format!("http://localhost:{port}/")}))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("from origin"), "{}", out.content);

    let mut events = Vec::new();
    for _ in 0..100 {
        events.extend(proxy.shared().drain_events());
        if events.iter().any(|event| event.allowed()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        events
            .iter()
            .any(|event| event.allowed() && event.label.as_deref() == Some("call-env")),
        "the fetch must have gone through the call's gateway: {events:?}"
    );
}
