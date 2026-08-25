//! Shared mock HTTP server for provider verify tests.
//!
//! The old per-provider `mock_server` helpers read a single partial chunk of
//! the request, wrote the response, and dropped the socket. Any request bytes
//! still queued in the receive buffer at that point make macOS close the
//! connection with a TCP RST instead of a FIN, which intermittently surfaces
//! in the client as `read_exact` failing with `EINVAL` — more often under
//! parallel test load, when a request is more likely to arrive split across
//! segments. Reading the full request (headers through `\r\n\r\n` plus any
//! Content-Length body) and shutting the write side down gracefully closes
//! the connection cleanly instead.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener};
use std::thread;

/// Serve one HTTP connection per status in `statuses` on an ephemeral port,
/// responding with `body` each time. Returns the base URL for the server.
///
/// The connection is drained of its request and closed gracefully, so the
/// client never sees a RST mid-response (see module docs).
pub(crate) fn serve(statuses: Vec<u16>, body: &str) -> Option<String> {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("skipping: this host denies binding a mock listener");
            return None;
        }
        Err(e) => panic!("bind mock listener: {e}"),
    };
    let address = listener.local_addr().unwrap();
    let body = body.to_string();
    thread::spawn(move || {
        for status in statuses {
            let (mut stream, _) = listener.accept().unwrap();
            drain_request(&mut stream);
            let response = format!(
                "HTTP/1.1 {status} test\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            let _ = stream.shutdown(Shutdown::Write);
        }
    });
    Some(format!("http://{address}/"))
}

/// Drain a single HTTP request so no unread bytes remain in the socket's
/// receive buffer when the connection is closed. Reads through the header
/// terminator, then any body the Content-Length header promises.
fn drain_request(stream: &mut std::net::TcpStream) {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(count) => {
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    break position + 4;
                }
            }
            Err(_) => return,
        }
    };
    let content_length = String::from_utf8_lossy(&bytes[..header_end])
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
        }
    }
}
