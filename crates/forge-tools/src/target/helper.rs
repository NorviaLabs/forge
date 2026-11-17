//! The file helper's wire format: how Forge talks to the long-lived `bash`
//! it keeps on an SSH host, so a file operation costs one network round trip
//! instead of a new SSH channel.
//!
//! * Ready: the helper's first line is `FORGE-HELPER-<nonce> ready` (or
//!   `FORGE-HELPER-<nonce> unavailable <reason>`). Anything a shell startup
//!   file prints before it is skipped, up to a limit.
//! * Request: `F <argc> <input>\n`, then each argument as `<len>\n<bytes>\n`
//!   (lengths in bytes), then, when `<input>` is `1`, one line of base64:
//!   the content a write puts in the file. Arguments are data the helper
//!   reads by length; no shell parses them.
//! * Reply: `R <status> <outlen> <errlen>\n`, then exactly that many bytes of
//!   the operation's stdout and stderr.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

use super::RunOutput;
use crate::ToolError;

/// Helpers a session keeps on its host at most: parallel file operations
/// (reads, searches) each get their own.
pub(crate) const POOL_SIZE: usize = 4;
/// What a helper may print before its ready line (shell startup noise).
const READY_LIMIT: usize = 64 * 1024;
/// Input above this goes over its own SSH command: the helper reads its
/// requests a byte at a time, which is slow for large content.
pub(crate) const MAX_INPUT: usize = 64 * 1024;
/// Helpers that fail to start in a row before the session stops trying.
const MAX_START_FAILURES: u32 = 3;

/// One helper: a `bash` on the host, reached over the session's connection.
pub(crate) struct Helper {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    generation: u64,
    /// Its process (and process group) on the host, when it said.
    pid: Option<u32>,
    /// A request is under way: dropped now, the helper is abandoned.
    busy: bool,
    /// Stops an abandoned helper's process group on the host.
    on_abandon: Option<Abandon>,
}

/// What stops an abandoned helper on the host, given its process id.
pub(crate) type Abandon = std::sync::Arc<dyn Fn(u32) + Send + Sync>;

impl Drop for Helper {
    fn drop(&mut self) {
        // Dropped mid-request (a cancelled tool call, a timeout, an error):
        // what it was running goes on, writing to a file, unless stopped.
        if self.busy {
            if let (Some(pid), Some(stop)) = (self.pid, self.on_abandon.as_ref()) {
                stop(pid);
            }
        }
    }
}

impl Helper {
    /// Start one from `command` (the `ssh` that runs the script in serve
    /// mode) and wait for its ready line.
    pub(crate) async fn start(
        mut command: tokio::process::Command,
        nonce: &str,
        within: Duration,
        generation: u64,
        on_abandon: Abandon,
    ) -> Result<Result<Helper, String>, ToolError> {
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|error| broken(&format!("could not start: {error}")))?;
        let stdin = child.stdin.take().expect("piped");
        let mut stdout = BufReader::new(child.stdout.take().expect("piped"));
        let ready = tokio::time::timeout(within, wait_ready(&mut stdout, nonce, READY_LIMIT))
            .await
            .map_err(|_| broken("did not start in time"))??;
        Ok(match ready {
            Ready::Ready(pid) => Ok(Helper {
                child,
                stdin,
                stdout,
                generation,
                pid,
                busy: false,
                on_abandon: Some(on_abandon),
            }),
            Ready::Unavailable(reason) => Err(reason),
        })
    }

    /// One request and its reply. On error the helper's state is unknown:
    /// the caller drops it.
    pub(crate) async fn call(
        &mut self,
        args: &[&str],
        input: Option<&[u8]>,
        cap: usize,
    ) -> Result<RunOutput, ToolError> {
        let request = encode_request(args, input)?;
        self.busy = true;
        self.stdin
            .write_all(&request)
            .await
            .map_err(|_| broken("stopped"))?;
        self.stdin.flush().await.map_err(|_| broken("stopped"))?;
        let reply = read_reply(&mut self.stdout, cap).await?;
        self.busy = false;
        Ok(reply)
    }

    /// Whether its local `ssh` is still running (a helper that ended on the
    /// host takes it down with it).
    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

/// A session's helpers. Each serves one request at a time; one whose
/// request did not finish cleanly is dropped, never reused.
#[derive(Default)]
pub(crate) struct HelperPool {
    /// With when each was last used; the most recent is last.
    idle: Mutex<Vec<(Helper, std::time::Instant)>>,
    permits: PoolPermits,
    disabled: AtomicBool,
    start_failures: AtomicU32,
    generation: AtomicU64,
}

struct PoolPermits(tokio::sync::Semaphore);

impl Default for PoolPermits {
    fn default() -> Self {
        Self(tokio::sync::Semaphore::new(POOL_SIZE))
    }
}

impl std::fmt::Debug for HelperPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelperPool")
            .field("disabled", &self.disabled.load(Ordering::Relaxed))
            .finish()
    }
}

impl HelperPool {
    /// Wait for a free slot (at most [`POOL_SIZE`] requests at once).
    pub(crate) async fn slot(&self) -> tokio::sync::SemaphorePermit<'_> {
        self.permits
            .0
            .acquire()
            .await
            .expect("the pool's semaphore is never closed")
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// An idle helper that is still running, if there is one.
    pub(crate) fn take_idle(&self) -> Option<Helper> {
        let mut idle = self.idle.lock().expect("helper pool lock");
        while let Some((mut helper, _)) = idle.pop() {
            if helper.alive() && helper.generation == self.generation() {
                return Some(helper);
            }
        }
        None
    }

    /// A helper whose request finished cleanly goes back for the next one.
    /// Returns how many are idle now.
    pub(crate) fn give_back(&self, helper: Helper) -> usize {
        let mut idle = self.idle.lock().expect("helper pool lock");
        if helper.generation == self.generation() && !self.is_disabled() {
            idle.push((helper, std::time::Instant::now()));
        }
        idle.len()
    }

    /// Close idle helpers unused for `after`, keeping the most recently
    /// used one: each helper is an SSH session, and a host allows only so
    /// many on one connection.
    pub(crate) fn prune(&self, after: Duration) {
        let mut idle = self.idle.lock().expect("helper pool lock");
        let Some(newest) = idle.len().checked_sub(1) else {
            return;
        };
        let mut index = 0;
        idle.retain(|(_, since)| {
            let keep = index == newest || since.elapsed() < after;
            index += 1;
            keep
        });
    }

    /// Stop every idle helper (the connection was restarted, or the
    /// session closes); helpers busy now are not given back.
    pub(crate) fn clear(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.idle.lock().expect("helper pool lock").clear();
    }

    pub(crate) fn is_disabled(&self) -> bool {
        self.disabled.load(Ordering::SeqCst)
    }

    pub(crate) fn disable(&self, host: &str, reason: &str) {
        if !self.disabled.swap(true, Ordering::SeqCst) {
            let reason = loggable(reason);
            tracing::warn!(
                host,
                reason,
                "the file helper cannot run on this host; file operations open an SSH command each"
            );
        }
        self.clear();
    }

    pub(crate) fn started(&self) {
        self.start_failures.store(0, Ordering::SeqCst);
    }

    pub(crate) fn failed_to_start(&self, host: &str, error: &ToolError) {
        if self.start_failures.fetch_add(1, Ordering::SeqCst) + 1 >= MAX_START_FAILURES {
            self.disable(host, &error.to_string());
        }
    }
}

/// What the helper said when it started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ready {
    /// With the helper's process id on the host (its process group), when
    /// it said: what Forge kills when it abandons a request.
    Ready(Option<u32>),
    /// It cannot serve on this host (e.g. no `base64`).
    Unavailable(String),
}

/// Text from the host, safe for the log: control characters escaped.
pub(crate) fn loggable(text: &str) -> String {
    text.chars().flat_map(char::escape_default).collect()
}

/// One request, framed.
pub(crate) fn encode_request(args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>, ToolError> {
    let mut request = format!("F {} {}\n", args.len(), u8::from(input.is_some())).into_bytes();
    for arg in args {
        if arg.contains('\0') {
            return Err(ToolError::Execution(
                "an argument for the host contains a NUL byte".into(),
            ));
        }
        request.extend_from_slice(format!("{}\n", arg.len()).as_bytes());
        request.extend_from_slice(arg.as_bytes());
        request.push(b'\n');
    }
    if let Some(input) = input {
        request.extend_from_slice(
            base64::engine::general_purpose::STANDARD
                .encode(input)
                .as_bytes(),
        );
        request.push(b'\n');
    }
    Ok(request)
}

fn broken(what: &str) -> ToolError {
    ToolError::Execution(format!("the file helper on the host {what}"))
}

/// Read up to the helper's ready line for `nonce`, skipping at most `limit`
/// bytes of anything else.
pub(crate) async fn wait_ready<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    nonce: &str,
    limit: usize,
) -> Result<Ready, ToolError> {
    let ready = format!("FORGE-HELPER-{nonce} ready");
    let unavailable = format!("FORGE-HELPER-{nonce} unavailable ");
    let mut seen = 0usize;
    loop {
        let mut line = Vec::new();
        // Bounded: never more than what is left of `limit`, line or not.
        let left = (limit + 1).saturating_sub(seen) as u64;
        let read = (&mut *reader)
            .take(left)
            .read_until(b'\n', &mut line)
            .await
            .map_err(|error| broken(&format!("could not start: {error}")))?;
        if read == 0 {
            return Err(broken("stopped before it was ready"));
        }
        seen += read;
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\n', '\r']);
        if let Some(rest) = text.strip_prefix(&ready) {
            if rest.is_empty() {
                return Ok(Ready::Ready(None));
            }
            return match rest
                .strip_prefix(' ')
                .and_then(|pid| pid.parse::<u32>().ok())
            {
                Some(pid) if pid > 1 => Ok(Ready::Ready(Some(pid))),
                _ => Err(broken(&format!("answered `{}`", loggable(text)))),
            };
        }
        if let Some(reason) = text.strip_prefix(&unavailable) {
            return Ok(Ready::Unavailable(reason.to_string()));
        }
        if seen > limit {
            return Err(broken("printed too much before it was ready"));
        }
    }
}

/// Read one reply; a declared length above `cap` is an error.
pub(crate) async fn read_reply<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    cap: usize,
) -> Result<RunOutput, ToolError> {
    let mut header = Vec::new();
    (&mut *reader)
        .take(128)
        .read_until(b'\n', &mut header)
        .await
        .map_err(|error| broken(&format!("stopped answering: {error}")))?;
    if header.last() != Some(&b'\n') {
        return Err(broken("stopped answering"));
    }
    let header = String::from_utf8_lossy(&header[..header.len() - 1]).into_owned();
    let mut words = header.split(' ');
    let number = |word: Option<&str>| -> Result<u64, ToolError> {
        word.filter(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|word| word.parse().ok())
            .ok_or_else(|| broken(&format!("answered `{header}`")))
    };
    if words.next() != Some("R") {
        return Err(broken(&format!("answered `{header}`")));
    }
    let status = number(words.next())?;
    let out_len = number(words.next())?;
    let err_len = number(words.next())?;
    if words.next().is_some() {
        return Err(broken(&format!("answered `{header}`")));
    }
    if out_len > cap as u64 || err_len > cap as u64 {
        return Err(broken("answered with more than Forge reads"));
    }
    let mut stdout = vec![0; out_len as usize];
    let mut stderr = vec![0; err_len as usize];
    reader
        .read_exact(&mut stdout)
        .await
        .map_err(|_| broken("stopped mid-answer"))?;
    reader
        .read_exact(&mut stderr)
        .await
        .map_err(|_| broken("stopped mid-answer"))?;
    Ok(RunOutput {
        status: i32::try_from(status).ok(),
        stdout,
        stderr,
        timed_out: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_carries_each_argument_by_length() {
        let request = encode_request(&["read", "/a b\nc", "'q' ✓", ""], None).unwrap();
        assert_eq!(
            request,
            "F 4 0\n4\nread\n6\n/a b\nc\n7\n'q' ✓\n0\n\n".as_bytes()
        );
    }

    #[test]
    fn a_write_sends_its_content_as_one_base64_line() {
        let request = encode_request(&["write", "/f"], Some(b"\x00\xffhi\n")).unwrap();
        assert_eq!(request, b"F 2 1\n5\nwrite\n2\n/f\nAP9oaQo=\n".to_vec());
        let empty = encode_request(&["write", "/f"], Some(b"")).unwrap();
        assert_eq!(empty, b"F 2 1\n5\nwrite\n2\n/f\n\n".to_vec());
    }

    #[test]
    fn a_hosts_reason_is_logged_without_its_control_characters() {
        let logged = loggable("no base64\u{1b}[2J\r\nforged line");
        assert!(!logged.chars().any(char::is_control), "{logged:?}");
        assert!(logged.starts_with("no base64"));
    }

    #[test]
    fn an_argument_with_a_nul_byte_is_refused() {
        assert!(encode_request(&["read", "/a\0b"], None).is_err());
    }

    async fn ready(bytes: &[u8], limit: usize) -> Result<Ready, ToolError> {
        let mut reader = tokio::io::BufReader::new(bytes);
        wait_ready(&mut reader, "n0nce", limit).await
    }

    #[tokio::test]
    async fn only_this_helpers_ready_line_counts() {
        assert_eq!(
            ready(
                b"Welcome!\nFORGE-HELPER-guess ready 1\nFORGE-HELPER-n0nce ready 4242\n",
                1024
            )
            .await
            .unwrap(),
            Ready::Ready(Some(4242))
        );
        assert_eq!(
            ready(b"FORGE-HELPER-n0nce ready\n", 1024).await.unwrap(),
            Ready::Ready(None)
        );
        assert!(
            ready(b"FORGE-HELPER-n0nce ready 12ab\n", 1024)
                .await
                .is_err(),
            "a process id is digits or nothing"
        );
        assert_eq!(
            ready(b"FORGE-HELPER-n0nce unavailable base64\n", 1024)
                .await
                .unwrap(),
            Ready::Unavailable("base64".into())
        );
        assert!(
            ready(b"FORGE-HELPER-guess ready\n", 1024).await.is_err(),
            "EOF first"
        );
        let noisy = [b"x".repeat(2048), b"\nFORGE-HELPER-n0nce ready\n".to_vec()].concat();
        assert!(ready(&noisy, 1024).await.is_err(), "too much noise");
    }

    async fn reply(bytes: &[u8], cap: usize) -> Result<RunOutput, ToolError> {
        let mut reader = bytes;
        read_reply(&mut reader, cap).await
    }

    #[tokio::test]
    async fn a_reply_is_read_by_its_lengths_and_keeps_binary_bytes() {
        let out = reply(b"R 3 5 3\nhe\0lo\ner", 1024).await.unwrap();
        assert_eq!(out.status, Some(3));
        assert_eq!(out.stdout, b"he\0lo");
        assert_eq!(out.stderr, b"\ner");
        assert!(!out.timed_out);
        // The next reply starts right after the bytes this one declared.
        let mut two: &[u8] = b"R 0 1 0\nxR 1 0 1\ny";
        assert_eq!(read_reply(&mut two, 1024).await.unwrap().stdout, b"x");
        let second = read_reply(&mut two, 1024).await.unwrap();
        assert_eq!((second.status, second.stderr), (Some(1), b"y".to_vec()));
    }

    #[tokio::test]
    async fn a_short_oversized_or_garbled_reply_is_an_error() {
        for bad in [
            &b"R 0 5 0\nabc"[..],
            b"R 0 2000 0\n",
            b"X 0 0 0\n",
            b"R zero 0 0\n",
            b"R 0 -1 0\n",
            b"",
        ] {
            assert!(
                reply(bad, 1024).await.is_err(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }
}
