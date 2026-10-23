//! `forge __egress-connect`, the SSH `ProxyCommand` helper, against a real
//! in-process gateway and a local echo upstream.

use std::time::Duration;

use assert_cmd::Command;
use forge_tools::egress::{EgressPolicy, EgressProxy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Echoes every byte back, then closes once the client half-closes.
async fn echo_upstream() -> Option<u16> {
    let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("skipping: this host denies binding a listener");
            return None;
        }
        Err(e) => panic!("bind echo upstream: {e}"),
    };
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => {
                            if sock.write_all(&buf[..read]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    Some(port)
}

async fn gateway(allow: &str) -> EgressProxy {
    let mut policy = EgressPolicy::new();
    policy.allow(allow);
    EgressProxy::start(policy).await.unwrap()
}

/// Run the helper off the async runtime, which keeps serving the gateway.
async fn egress_connect(args: Vec<String>, stdin: &'static str) -> std::process::Output {
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("forge")
            .unwrap()
            .arg("__egress-connect")
            .args(args)
            .write_stdin(stdin)
            .timeout(Duration::from_secs(20))
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn egress_connect_tunnels_an_allowed_target() {
    let Some(port) = echo_upstream().await else {
        return;
    };
    let proxy = gateway("127.0.0.1").await;
    let message = "SSH-2.0-test\r\nbytes pass through untouched\n";
    let out = egress_connect(
        vec![
            proxy.tcp_addr().to_string(),
            "127.0.0.1".into(),
            port.to_string(),
        ],
        message,
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), message);

    let events = proxy.shared().drain_events();
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(events[0].allowed());
    assert_eq!(events[0].bytes_up as usize, message.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn egress_connect_reports_a_denial_and_fails() {
    let proxy = gateway("127.0.0.1").await;
    let out = egress_connect(
        vec![
            proxy.tcp_addr().to_string(),
            "blocked.example".into(),
            "22".into(),
        ],
        "",
    )
    .await;
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "nothing may reach stdout");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("403 Forge Sandbox Denied")
            && stderr.contains("host blocked.example is not allowed"),
        "{stderr}"
    );
    assert_eq!(
        proxy.shared().take_denied_host().as_deref(),
        Some("blocked.example")
    );
}

#[test]
fn egress_connect_is_hidden_and_validates_its_arguments() {
    let help = Command::cargo_bin("forge")
        .unwrap()
        .arg("--help")
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&help.stdout).contains("egress-connect"));

    for args in [
        vec![],
        vec!["127.0.0.1:1", "host"],
        vec!["127.0.0.1:1", "host", "not-a-port"],
    ] {
        let out = Command::cargo_bin("forge")
            .unwrap()
            .arg("__egress-connect")
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
}
