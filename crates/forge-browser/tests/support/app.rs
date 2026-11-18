//! A fixture web app served on a Unix socket behind the real ingress
//! relay, the way a sandboxed app is reached from the host.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use forge_tools::ingress::IngressRelay;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn page(status: &str, body: &str) -> (String, String, String) {
    (
        status.into(),
        "text/html; charset=utf-8".into(),
        body.into(),
    )
}

/// The fixture app's answer for `path`. `other` is a loopback port of
/// something else on the host that the page must not reach.
fn route(path: &str, other: u16) -> (String, String, String) {
    match path {
        "/" => page(
            "200 OK",
            "<!doctype html><title>Home — Acme</title><h1>Welcome</h1>\
             <a href=\"/settings\">Settings</a>",
        ),
        "/throws" => page(
            "200 OK",
            "<!doctype html><title>Throws</title><p>hi</p>\
             <script>null.classList; </script>\
             <script>throw new TypeError('boom-exception')</script>",
        ),
        "/console" => page(
            "200 OK",
            "<!doctype html><title>Console</title><p>hi</p>\
             <script>console.error('console boom', 42); console.warn('just a warning')</script>",
        ),
        "/missing-asset" => page(
            "200 OK",
            "<!doctype html><title>Asset</title><img src=\"/nope.png\" alt=\"x\">",
        ),
        "/500" => page(
            "500 Internal Server Error",
            "<!doctype html><title>Error</title><p>Internal Server Error</p>",
        ),
        "/external" => page(
            "200 OK",
            "<!doctype html><title>External</title>\
             <link rel=\"stylesheet\" href=\"https://fonts.googleapis.com/css?family=Inter\">\
             <p>content</p><script>fetch('http://example.com/track').catch(() => {})</script>",
        ),
        "/loopback" => page(
            "200 OK",
            &format!(
                "<!doctype html><title>Loopback</title><p>content</p><script>\
                 fetch('http://127.0.0.1:{other}/secret').catch(() => {{}});\
                 fetch('http://localhost:{other}/secret').catch(() => {{}});\
                 </script>"
            ),
        ),
        "/form" => page(
            "200 OK",
            "<!doctype html><title>Greeter</title>\
             <label>Name <input id=\"name\"></label>\
             <button id=\"greet\" onclick=\"document.getElementById('out').textContent = 'Hello, ' + document.getElementById('name').value\">Greet</button>\
             <p id=\"out\"></p>\
             <script>document.getElementById('name').addEventListener('keydown', e => { if (e.key === 'Enter') document.getElementById('greet').click(); })</script>",
        ),
        "/late" => page(
            "200 OK",
            "<!doctype html><title>Late</title><div id=\"app\">Loading…</div>\
             <script>fetch('/api/data').then(r => r.json()).then(d => { document.getElementById('app').textContent = d.message; })</script>",
        ),
        "/api/data" => (
            "200 OK".into(),
            "application/json".into(),
            "{\"message\":\"Loaded data\"}".into(),
        ),
        "/mobile" => page(
            "200 OK",
            "<!doctype html><title>Menu</title>\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
             <style>#menu{display:none} @media (max-width: 600px){#menu{display:block}}</style>\
             <div id=\"menu\">Mobile menu</div><p>Always here</p>",
        ),
        "/click-fetch" => page(
            "200 OK",
            "<!doctype html><title>Click fetch</title>\
             <button onclick=\"fetch('/slow-missing')\">Load</button>",
        ),
        "/alert" => page(
            "200 OK",
            "<!doctype html><title>Alert</title><p>after the alert</p><script>alert('hello')</script>",
        ),
        _ => page("404 Not Found", "<!doctype html><title>Not found</title>"),
    }
}

pub struct App {
    pub origin: SocketAddr,
    _relay: IngressRelay,
    _dir: tempfile::TempDir,
    /// Requests something else on the host received from the page.
    pub other_hits: Arc<AtomicUsize>,
    pub other_port: u16,
}

/// A TCP server standing for another service on the host.
async fn other_service() -> (u16, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            seen.fetch_add(1, Ordering::SeqCst);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nsecret")
                .await;
        }
    });
    (port, hits)
}

pub async fn app() -> App {
    let (other_port, other_hits) = other_service().await;
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("app.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).await.unwrap_or(0) == 0 {
                    return;
                }
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0
                        || line.trim_end().is_empty()
                    {
                        break;
                    }
                }
                let path = request_line.split_whitespace().nth(1).unwrap_or("/");
                let path = path.split('?').next().unwrap_or("/");
                if path == "/api/data" || path.starts_with("/slow") {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                let (status, kind, body) = route(path, other_port);
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = write.write_all(response.as_bytes()).await;
                let _ = write.shutdown().await;
            });
        }
    });
    let relay = IngressRelay::start(socket).await.unwrap();
    App {
        origin: relay.addr(),
        _relay: relay,
        _dir: dir,
        other_hits,
        other_port,
    }
}
