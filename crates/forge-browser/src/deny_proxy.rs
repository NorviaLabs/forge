//! The web check browser's proxy: it lets nothing through.
//!
//! The checker browser reaches the app directly and sends everything else
//! here (`--proxy-server` with only the app's origin bypassed). Each request
//! is answered `403` and its host recorded, so a check can say which outside
//! hosts the page tried to reach.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A host the page tried to reach, and how often.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    /// `host` or `host:port` as the request named it (the port is kept when
    /// it is not the scheme's default).
    pub host: String,
    pub requests: u32,
}

/// Most distinct hosts remembered between two looks.
pub const MAX_HOSTS: usize = 50;

/// Most connections handled at once; more are closed at once.
pub const MAX_CONNECTIONS: usize = 64;

/// The longest request head read before the answer.
const MAX_HEAD: usize = 8 * 1024;

const REFUSAL: &[u8] = b"HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain\r\n\
Content-Length: 47\r\nConnection: close\r\n\r\nForge web check: only the app is reachable here";

type Record = Arc<Mutex<Vec<Blocked>>>;

/// A running deny proxy on a random loopback port; stops when dropped.
pub struct DenyProxy {
    addr: SocketAddr,
    blocked: Record,
    task: tokio::task::JoinHandle<()>,
}

impl DenyProxy {
    pub async fn start() -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let blocked: Record = Arc::default();
        let record = blocked.clone();
        let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else {
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            continue;
                        };
                        let Ok(permit) = permits.clone().try_acquire_owned() else {
                            continue; // dropped: closed at once
                        };
                        let record = record.clone();
                        connections.spawn(async move {
                            let _permit = permit;
                            refuse(stream, record).await;
                        });
                    }
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Ok(Self {
            addr,
            blocked,
            task,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The hosts refused since the last call, first seen first.
    pub fn take_blocked(&self) -> Vec<Blocked> {
        std::mem::take(
            &mut *self
                .blocked
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }
}

impl Drop for DenyProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Read the request head, record its host, answer 403.
async fn refuse(mut stream: tokio::net::TcpStream, record: Record) {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    let read_head = async {
        while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < MAX_HEAD {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(read) => head.extend_from_slice(&buf[..read]),
            }
        }
    };
    if tokio::time::timeout(Duration::from_secs(5), read_head)
        .await
        .is_err()
    {
        return;
    }
    let Some(host) = requested_host(&head) else {
        return;
    };
    {
        let mut hosts = record
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(blocked) = hosts.iter_mut().find(|blocked| blocked.host == host) {
            blocked.requests += 1;
        } else if hosts.len() < MAX_HOSTS {
            hosts.push(Blocked { host, requests: 1 });
        }
    }
    let _ = stream.write_all(REFUSAL).await;
    let _ = stream.shutdown().await;
}

/// The host a proxy request names: `CONNECT host:port` or an absolute URI.
fn requested_host(head: &[u8]) -> Option<String> {
    let line = head.split(|&b| b == b'\r' || b == b'\n').next()?;
    let line = std::str::from_utf8(line).ok()?;
    let mut parts = line.split(' ');
    let method = parts.next()?;
    let target = parts.next()?;
    if !parts.next()?.starts_with("HTTP/") {
        return None;
    }
    let (authority, default_port) = if method == "CONNECT" {
        (target, 443)
    } else {
        let (scheme, rest) = target.split_once("://")?;
        let default_port = match scheme.to_ascii_lowercase().as_str() {
            "http" | "ws" => 80,
            "https" | "wss" => 443,
            _ => return None,
        };
        (rest.split(['/', '?', '#']).next()?, default_port)
    };
    let authority = authority.rsplit('@').next()?.to_ascii_lowercase();
    if authority.is_empty()
        || !authority
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']' | '_'))
    {
        return None;
    }
    // Split off a port, minding IPv6 brackets.
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port))
            if !port.is_empty()
                && port.chars().all(|c| c.is_ascii_digit())
                && (!host.contains(':') || host.ends_with(']')) =>
        {
            (host.to_string(), port.parse::<u16>().ok())
        }
        _ => (authority.clone(), None),
    };
    Some(match port {
        Some(port) if port != default_port => format!("{host}:{port}"),
        _ => host,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    async fn ask(proxy: &DenyProxy, request: &str) -> String {
        let mut stream = TcpStream::connect(proxy.addr()).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut answer = String::new();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.read_to_string(&mut answer),
        )
        .await;
        answer
    }

    #[tokio::test]
    async fn a_tunnel_is_refused_and_its_host_recorded() {
        let proxy = DenyProxy::start().await.unwrap();
        let answer = ask(
            &proxy,
            "CONNECT fonts.googleapis.com:443 HTTP/1.1\r\nHost: fonts.googleapis.com:443\r\n\r\n",
        )
        .await;
        assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
        assert_eq!(
            proxy.take_blocked(),
            [Blocked {
                host: "fonts.googleapis.com".into(),
                requests: 1
            }]
        );
        assert!(proxy.take_blocked().is_empty(), "taken once");
    }

    #[tokio::test]
    async fn a_plain_request_is_refused_and_counted_per_host() {
        let proxy = DenyProxy::start().await.unwrap();
        for path in ["/a", "/b"] {
            let answer = ask(
                &proxy,
                &format!("GET http://example.com{path} HTTP/1.1\r\nHost: example.com\r\n\r\n"),
            )
            .await;
            assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
        }
        ask(
            &proxy,
            "GET http://127.0.0.1:6379/ HTTP/1.1\r\nHost: 127.0.0.1:6379\r\n\r\n",
        )
        .await;
        assert_eq!(
            proxy.take_blocked(),
            [
                Blocked {
                    host: "example.com".into(),
                    requests: 2
                },
                Blocked {
                    host: "127.0.0.1:6379".into(),
                    requests: 1
                },
            ]
        );
    }

    /// The default port says nothing; any other port is part of the host.
    #[tokio::test]
    async fn default_ports_are_dropped_from_the_host() {
        let proxy = DenyProxy::start().await.unwrap();
        ask(&proxy, "CONNECT localhost:8443 HTTP/1.1\r\n\r\n").await;
        ask(&proxy, "GET http://example.org:80/x HTTP/1.1\r\n\r\n").await;
        ask(&proxy, "GET ws://[::1]:9000/socket HTTP/1.1\r\n\r\n").await;
        let hosts: Vec<String> = proxy.take_blocked().into_iter().map(|b| b.host).collect();
        assert_eq!(hosts, ["localhost:8443", "example.org", "[::1]:9000"]);
    }

    #[tokio::test]
    async fn garbage_is_closed_without_a_record() {
        let proxy = DenyProxy::start().await.unwrap();
        ask(&proxy, "\x16\x03\x01 not http\r\n\r\n").await;
        assert!(proxy.take_blocked().is_empty());
    }

    /// A flood of connections cannot pile up: past the cap they are closed
    /// at once, and the ones held keep their place.
    #[tokio::test]
    async fn connections_past_the_cap_are_closed_at_once() {
        let proxy = DenyProxy::start().await.unwrap();
        let mut held = Vec::new();
        for _ in 0..MAX_CONNECTIONS {
            // Says nothing, so the proxy keeps waiting for its request.
            held.push(TcpStream::connect(proxy.addr()).await.unwrap());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let mut extra = TcpStream::connect(proxy.addr()).await.unwrap();
        let mut buf = [0u8; 16];
        let closed =
            tokio::time::timeout(std::time::Duration::from_secs(1), extra.read(&mut buf)).await;
        assert!(
            matches!(closed, Ok(Ok(0)) | Ok(Err(_))),
            "past the cap: closed at once, not held: {closed:?}"
        );
        let mut first = held.remove(0);
        let still =
            tokio::time::timeout(std::time::Duration::from_millis(200), first.read(&mut buf)).await;
        assert!(still.is_err(), "a held connection keeps its place");
    }

    #[tokio::test]
    async fn only_so_many_hosts_are_remembered() {
        let proxy = DenyProxy::start().await.unwrap();
        for index in 0..(MAX_HOSTS + 5) {
            ask(
                &proxy,
                &format!("CONNECT host{index}.example:443 HTTP/1.1\r\n\r\n"),
            )
            .await;
        }
        assert_eq!(proxy.take_blocked().len(), MAX_HOSTS);
    }
}
