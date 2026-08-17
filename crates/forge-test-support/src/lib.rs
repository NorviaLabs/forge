//! Test-only fixtures shared across crates.
//!
//! Two families live here, both dependency-free beyond `std`:
//!
//! - [`mock_http`]: a scripted-response HTTP server over a real TCP socket.
//!   Being a real socket rather than an interceptor means it works for both
//!   sync (`ureq`) and async (`reqwest`) clients — which matters since
//!   `forge-connect` uses both. Moved out of `forge-connect`'s own test module
//!   (crates/forge-connect#74) so `forge-model` and `forge-mcp` can reach it
//!   without depending on `forge-connect`.
//! - [`git`], [`init_repo`] and [`init_repo_with_commit`]: the git working
//!   copy that most crates' tests need before they can exercise
//!   repository-scoped behavior. Every crate that needed one had grown its own
//!   private copy of the same three `git` invocations.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::thread;
use std::time::Duration;

/// Run `git <args>` in `dir`, asserting that it succeeded.
///
/// Panics with the failing argument list rather than a bare status assert, so
/// a broken fixture names itself instead of surfacing as a confusing failure
/// in whatever test happened to call it.
pub fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("git must be on PATH for these tests");
    assert!(status.success(), "git {args:?} failed in {dir:?}");
}

/// Initialize an empty git repository in `dir` with a deterministic branch
/// name and committer identity.
///
/// The identity matters because a test host may have no global git config, and
/// `--initial-branch=main` because the default branch name is a host setting.
/// There is no commit, so `HEAD` is unborn — use [`init_repo_with_commit`]
/// when the test needs a real `HEAD`.
pub fn init_repo(dir: &Path) {
    git(dir, &["init", "--initial-branch=main", "-q"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
}

/// [`init_repo`] plus a single committed file, so the repository has a real
/// `HEAD` commit for tests that branch, stash, or add a worktree.
pub fn init_repo_with_commit(dir: &Path) {
    init_repo(dir);
    std::fs::write(dir.join("f.txt"), "x").expect("write fixture file");
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

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

    fn git_stdout(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git must be on PATH for these tests");
        assert!(out.status.success(), "git {args:?} failed in {dir:?}");
        String::from_utf8(out.stdout).expect("git output is utf-8")
    }

    #[test]
    fn init_repo_creates_a_repository_on_main_with_an_identity() {
        let dir = tempfile::TempDir::new().unwrap();
        init_repo(dir.path());

        assert!(dir.path().join(".git").is_dir());
        assert_eq!(
            git_stdout(dir.path(), &["symbolic-ref", "--short", "HEAD"]).trim(),
            "main"
        );
        assert_eq!(
            git_stdout(dir.path(), &["config", "user.email"]).trim(),
            "test@example.com"
        );
        assert_eq!(
            git_stdout(dir.path(), &["config", "user.name"]).trim(),
            "Test"
        );
    }

    /// The distinction between the two fixtures: only the committing one
    /// leaves a resolvable `HEAD` behind.
    #[test]
    fn only_the_committing_fixture_leaves_a_head_commit() {
        let bare = tempfile::TempDir::new().unwrap();
        init_repo(bare.path());
        let unborn = std::process::Command::new("git")
            .arg("-C")
            .arg(bare.path())
            .args(["rev-parse", "--verify", "HEAD"])
            .output()
            .unwrap();
        assert!(!unborn.status.success(), "expected an unborn HEAD");

        let committed = tempfile::TempDir::new().unwrap();
        init_repo_with_commit(committed.path());
        assert!(!git_stdout(committed.path(), &["rev-parse", "HEAD"])
            .trim()
            .is_empty());
        assert_eq!(
            git_stdout(committed.path(), &["ls-files"]).trim(),
            "f.txt",
            "the fixture commit should track exactly one file"
        );
    }

    #[test]
    #[should_panic(expected = "failed in")]
    fn git_panics_with_the_failing_arguments() {
        let dir = tempfile::TempDir::new().unwrap();
        init_repo(dir.path());
        git(dir.path(), &["rev-parse", "--verify", "HEAD"]);
    }
}
