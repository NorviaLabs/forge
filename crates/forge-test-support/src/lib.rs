//! Test-only fixtures shared across crates.
//!
//! Currently just [`mock_http`]: a scripted-response HTTP server over a real
//! TCP socket. It has no dependencies beyond `std`, and being a real socket
//! rather than an interceptor means it works for both sync (`ureq`) and
//! async (`reqwest`) clients — which matters since `forge-connect` uses both.
//!
//! Moved out of `forge-connect`'s own test module (crates/forge-connect#74)
//! so `forge-model` and `forge-mcp` can reach it without depending on
//! `forge-connect`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

/// `(status, body, extra headers)`
pub type MockResponse = (u16, &'static str, Vec<(&'static str, &'static str)>);

/// Spawn a background thread that serves `responses` in order, one per
/// accepted connection, then returns the `http://host:port` base URL to
/// point a client at.
///
/// The listener thread never panics: a malformed or unreadable request just
/// closes the connection without a response, so a broken test surfaces as
/// the client-side HTTP call failing (connection reset / timeout inside the
/// client's own error handling), not as the whole test process hanging on a
/// dead thread.
pub fn mock_http(responses: Vec<MockResponse>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock listener");
    let addr = listener.local_addr().expect("mock listener local_addr");
    thread::spawn(move || {
        for (status, body, headers) in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            if read_http_request(&mut stream).is_none() {
                continue;
            }
            let mut response = format!(
                "HTTP/1.1 {status} test\r\ncontent-length: {}\r\n",
                body.len()
            );
            for (name, value) in headers {
                response.push_str(name);
                response.push_str(": ");
                response.push_str(value);
                response.push_str("\r\n");
            }
            response.push_str("connection: close\r\n\r\n");
            response.push_str(body);
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://{addr}")
}

/// Read a full HTTP request (headers plus, if present, a `Content-Length`
/// body) off `stream`. Unlike a fixed-size buffer, this doesn't truncate
/// requests larger than some constant, and it doesn't over-read and block on
/// requests smaller than one. Returns `None` if the connection closed or
/// errored before a full header block arrived.
fn read_http_request(stream: &mut TcpStream) -> Option<Vec<u8>> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        // Any real request's headers fit comfortably under 64KB; past that,
        // something is wrong and we should stop reading rather than grow
        // this buffer forever.
        if buf.len() > 64 * 1024 {
            return None;
        }
    };
    let content_length: usize = String::from_utf8_lossy(&buf[..header_end])
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())
                .flatten()
        })
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Some(buf)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_scripted_responses_in_order() {
        let base = mock_http(vec![
            (200, "first", vec![]),
            (404, "second", vec![("x-custom", "yes")]),
        ]);
        let first = ureq::get(&base).call().unwrap();
        assert_eq!(first.status(), 200);
        assert_eq!(first.into_string().unwrap(), "first");

        match ureq::get(&base).call() {
            Err(ureq::Error::Status(404, response)) => {
                assert_eq!(response.header("x-custom"), Some("yes"));
            }
            other => panic!("expected a 404 status error, got {other:?}"),
        }
    }

    #[test]
    fn reads_a_request_body_larger_than_the_old_fixed_buffer() {
        let base = mock_http(vec![(200, "ok", vec![])]);
        let big_body = "x".repeat(8_000);
        let response = ureq::post(&base).send_string(&big_body).unwrap();
        assert_eq!(response.status(), 200);
    }
}
