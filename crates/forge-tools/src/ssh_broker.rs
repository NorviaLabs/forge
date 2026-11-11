//! SSH for a confined git, run on the host by Forge.
//!
//! A confined git cannot use SSH itself: the user's keys and agent socket are
//! outside the sandbox, and must stay there. Running git unconfined instead
//! lets whatever the repository's config names (a hook, `core.sshCommand`)
//! run unconfined too.
//!
//! So git stays in the sandbox and only the SSH transport leaves it. Git is
//! told to use
//!
//! ```text
//! <forge executable> __ssh-broker <127.0.0.1:port> <token>
//! ```
//!
//! as its ssh. That client forwards the arguments git gave it, and git's
//! stdin, to the broker in the Forge process; the broker runs the real `ssh`
//! on the host and relays its output and exit status back. The keys never
//! enter the sandbox.
//!
//! The broker does not run what it is asked to. A confined process can speak
//! to it, so it accepts exactly the invocation git makes: a destination, the
//! few options git adds, and one of git's three service commands with one
//! quoted path. The ssh command line itself is composed by Forge from the
//! user's own settings, never from the repository's.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// The subcommand name, matched before argument parsing so it never appears
/// in `--help`.
pub const SUBCOMMAND: &str = "__ssh-broker";

const USAGE: &str = "usage: forge __ssh-broker <broker-host:port> <token> <ssh arguments...>";

/// Largest request header the broker reads.
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// Largest single frame either side sends.
pub(crate) const MAX_FRAME_BYTES: usize = 64 * 1024;

// Frame kinds. Client to broker:
const STDIN_DATA: u8 = 0;
const STDIN_EOF: u8 = 1;
// Broker to client:
pub(crate) const STDOUT_DATA: u8 = 1;
pub(crate) const STDERR_DATA: u8 = 2;
pub(crate) const EXIT: u8 = 3;

/// The git services a remote is asked to run.
const GIT_SERVICES: &[&str] = &["git-upload-pack", "git-receive-pack", "git-upload-archive"];

/// One ssh invocation as git makes it, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshRequest {
    pub port: Option<u16>,
    /// `-4` or `-6`.
    pub family: Option<char>,
    pub send_git_protocol: bool,
    /// `[user@]host`.
    pub destination: String,
    /// `git-upload-pack`, `git-receive-pack` or `git-upload-archive`.
    pub service: String,
    /// The whole remote command, exactly as git wrote it.
    pub command: String,
}

impl SshRequest {
    /// The host part of the destination.
    pub fn host(&self) -> &str {
        self.destination
            .rsplit_once('@')
            .map_or(self.destination.as_str(), |(_, host)| host)
    }

    /// Whether the request writes to the remote: a push, or an LFS upload.
    pub fn is_push(&self) -> bool {
        self.service == "git-receive-pack"
            || (self.service == "git-lfs-authenticate" && self.command.ends_with(" upload"))
    }
}

/// Check the arguments git passed to its ssh command. Anything git itself
/// does not produce is refused: the caller is a confined process, and the
/// command runs outside the sandbox with the user's keys.
pub fn parse_request(args: &[String]) -> Result<SshRequest, String> {
    let mut port = None;
    let mut family = None;
    let mut send_git_protocol = false;
    let mut rest = args.iter();
    let destination = loop {
        let arg = rest.next().ok_or("no destination")?;
        match arg.as_str() {
            "-4" => family = Some('4'),
            "-6" => family = Some('6'),
            "-p" => {
                let value = rest.next().ok_or("`-p` without a port")?;
                port = Some(
                    value
                        .parse::<u16>()
                        .map_err(|_| format!("invalid port {value:?}"))?,
                );
            }
            "-o" => {
                let value = rest.next().ok_or("`-o` without an option")?;
                if value != "SendEnv=GIT_PROTOCOL" {
                    return Err(format!("ssh option {value:?} is not one git sets"));
                }
                send_git_protocol = true;
            }
            other if other.starts_with('-') => {
                return Err(format!("ssh option {other:?} is not one git sets"));
            }
            _ => break arg.clone(),
        }
    };
    let valid_destination = !destination.is_empty()
        && destination
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@._-:[]%+".contains(c))
        && !destination.ends_with('@');
    if !valid_destination {
        return Err(format!("{destination:?} is not a destination"));
    }
    let command = rest.next().ok_or("no remote command")?.clone();
    if rest.next().is_some() {
        return Err("more than one remote command".into());
    }
    let (service, path) = command
        .split_once(' ')
        .ok_or("the remote command is not a git service")?;
    if service == "git-lfs-authenticate" {
        // Git LFS asks the remote for an HTTPS token this way:
        // `git-lfs-authenticate <path> download|upload`.
        let (path, operation) = path
            .rsplit_once(' ')
            .ok_or("git-lfs-authenticate without an operation")?;
        if !matches!(operation, "download" | "upload") {
            return Err(format!("{operation:?} is not an LFS operation"));
        }
        let plain = !path.is_empty()
            && path
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "/._-~".contains(c));
        if !plain && !is_one_quoted_word(path) {
            return Err("the repository path is not one plain or quoted word".into());
        }
    } else {
        if !GIT_SERVICES.contains(&service) {
            return Err(format!("{service:?} is not a git service"));
        }
        if !is_one_quoted_word(path) {
            return Err("the repository path is not quoted the way git quotes it".into());
        }
    }
    Ok(SshRequest {
        port,
        family,
        send_git_protocol,
        destination,
        service: service.to_string(),
        command,
    })
}

/// Whether `text` is exactly one word in git's shell quoting (`sq_quote`):
/// single-quoted runs, with `'` and `!` written as `'\''` and `'\!'`. A
/// remote shell reads such a word as one argument and nothing else, so the
/// path cannot smuggle a second command.
fn is_one_quoted_word(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    if bytes.first() != Some(&b'\'') {
        return false;
    }
    while i < bytes.len() {
        // An opening quote, then everything up to the closing quote.
        if bytes[i] != b'\'' {
            return false;
        }
        i += 1;
        while i < bytes.len() && bytes[i] != b'\'' {
            if bytes[i] == 0 || bytes[i] == b'\n' {
                return false;
            }
            i += 1;
        }
        if i == bytes.len() {
            return false; // unterminated
        }
        i += 1;
        // Either the end, or an escaped `'`/`!` followed by another run.
        if i == bytes.len() {
            return true;
        }
        if bytes[i] != b'\\' || !matches!(bytes.get(i + 1), Some(b'\'' | b'!')) {
            return false;
        }
        i += 2;
    }
    false
}

/// A running broker for one git invocation. Dropping it stops accepting.
pub struct SshBroker {
    addr: SocketAddr,
    token: String,
    accept: tokio::task::JoinHandle<()>,
}

impl Drop for SshBroker {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

/// Decides whether a checked request may run. Returning `Err` refuses it
/// with that message, which git prints.
pub type Authorize = Arc<
    dyn Fn(
            SshRequest,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

/// An [`Authorize`] that lets every checked request run.
pub fn authorize_all() -> Authorize {
    Arc::new(|_| Box::pin(async { Ok(()) }))
}

/// Produces the host ssh command line for a request, or why SSH cannot be
/// used. Called when a request arrives, so a git invocation that never uses
/// SSH costs nothing.
pub type Compose = Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
        + Send
        + Sync,
>;

impl SshBroker {
    /// Start a broker that runs the command `compose` returns for each
    /// accepted request.
    ///
    /// That command is a shell command line (the user's ssh, as Forge
    /// composed it); the request's own arguments are appended as separate,
    /// quoted words. `authorize` sees every request after it has been
    /// checked and before anything runs.
    pub async fn start(compose: Compose, authorize: Authorize) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let token = uuid_like_token();
        let expected = token.clone();
        let accept = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (compose, expected, authorize) = (
                    Arc::clone(&compose),
                    expected.clone(),
                    Arc::clone(&authorize),
                );
                tokio::spawn(async move {
                    let _ = serve(stream, compose, &expected, authorize).await;
                });
            }
        });
        Ok(Self {
            addr,
            token,
            accept,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// A broker that always runs `ssh_command`.
    pub async fn start_fixed(ssh_command: String, authorize: Authorize) -> io::Result<Self> {
        Self::start(
            Arc::new(move || {
                let command = ssh_command.clone();
                Box::pin(async move { Ok(command) })
            }),
            authorize,
        )
        .await
    }

    /// `GIT_SSH_COMMAND` for a git that should use this broker. `exe` is the
    /// Forge executable providing [`SUBCOMMAND`].
    pub fn git_ssh_command(&self, exe: &str) -> String {
        format!(
            "{} {SUBCOMMAND} {} {}",
            shell_quote(exe),
            self.addr,
            self.token
        )
    }
}

pub(crate) fn uuid_like_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    // Two independently keyed SipHash states: 128 bits no other local
    // process can predict, without a dependency for it.
    let mut token = String::new();
    for _ in 0..2 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos()),
        );
        token.push_str(&format!("{:016x}", hasher.finish()));
    }
    token
}

pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |diff, (x, y)| diff | (x ^ y))
            == 0
}

pub(crate) async fn write_frame<W>(writer: &mut W, kind: u8, payload: &[u8]) -> io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    writer.write_u8(kind).await?;
    writer.write_u32(payload.len() as u32).await?;
    writer.write_all(payload).await?;
    writer.flush().await
}

async fn refuse<W>(writer: &mut W, why: &str) -> io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let message = format!("forge: refusing git over SSH: {why}\n");
    write_frame(writer, STDERR_DATA, message.as_bytes()).await?;
    write_frame(writer, EXIT, &255i32.to_be_bytes()).await
}

async fn serve(
    stream: tokio::net::TcpStream,
    compose: Compose,
    token: &str,
    authorize: Authorize,
) -> io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Header: the token, GIT_PROTOCOL (may be empty), the argument count,
    // then one argument per line. Arguments git passes hold no newlines.
    let mut header = Vec::new();
    let mut line = String::new();
    let read_line = |line: &mut String, header: &mut Vec<String>| {
        header.push(line.trim_end_matches('\n').to_string());
        line.clear();
    };
    let mut total = 0;
    let mut count: Option<usize> = None;
    loop {
        let n = reader.read_line(&mut line).await?;
        total += n;
        if n == 0 || total > MAX_HEADER_BYTES {
            return refuse(&mut write_half, "malformed request").await;
        }
        read_line(&mut line, &mut header);
        if header.len() == 3 {
            count = header[2].parse().ok().filter(|count| *count <= 16);
            if count.is_none() {
                return refuse(&mut write_half, "malformed request").await;
            }
        }
        if count.is_some_and(|count| header.len() == 3 + count) {
            break;
        }
    }
    if !constant_time_eq(&header[0], token) {
        return refuse(&mut write_half, "not the git invocation this broker serves").await;
    }
    let git_protocol = header[1].clone();
    let request = match parse_request(&header[3..]) {
        Ok(request) => request,
        Err(why) => return refuse(&mut write_half, &why).await,
    };
    if let Err(why) = authorize(request.clone()).await {
        return refuse(&mut write_half, &why).await;
    }
    let ssh_command = match compose().await {
        Ok(command) => command,
        Err(why) => return refuse(&mut write_half, &why).await,
    };

    // `sh -c '<the user's ssh> "$@"' forge-ssh <checked words...>`: the
    // request's words are positional parameters, never parsed as shell.
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(format!("{ssh_command} \"$@\""))
        .arg("forge-ssh")
        .args(["-o", "BatchMode=yes"]);
    if let Some(family) = request.family {
        command.arg(format!("-{family}"));
    }
    if let Some(port) = request.port {
        command.arg("-p").arg(port.to_string());
    }
    let protocol_ok = !git_protocol.is_empty()
        && git_protocol
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "=:.,-_".contains(c));
    if request.send_git_protocol && protocol_ok {
        command
            .args(["-o", "SendEnv=GIT_PROTOCOL"])
            .env("GIT_PROTOCOL", &git_protocol);
    } else {
        command.env_remove("GIT_PROTOCOL");
    }
    command
        .arg("--")
        .arg(&request.destination)
        .arg(&request.command)
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return refuse(&mut write_half, &format!("cannot start ssh: {error}")).await,
    };
    let mut child_stdin = child.stdin.take();
    let mut child_stdout = child.stdout.take().expect("piped");
    let mut child_stderr = child.stderr.take().expect("piped");

    // Client frames to the child's stdin.
    let stdin_pump = tokio::spawn(async move {
        while let Ok(kind) = reader.read_u8().await {
            let Ok(len) = reader.read_u32().await else {
                break;
            };
            if kind != STDIN_DATA || len as usize > MAX_FRAME_BYTES {
                break;
            }
            let mut payload = vec![0; len as usize];
            if reader.read_exact(&mut payload).await.is_err() {
                break;
            }
            let Some(stdin) = child_stdin.as_mut() else {
                break;
            };
            if stdin.write_all(&payload).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
        // EOF frame, a closed connection or an error: the child sees EOF.
        drop(child_stdin.take());
    });

    let mut out = vec![0; 32 * 1024];
    let mut err = vec![0; 8 * 1024];
    let (mut out_open, mut err_open) = (true, true);
    while out_open || err_open {
        tokio::select! {
            read = child_stdout.read(&mut out), if out_open => match read {
                Ok(n) if n > 0 => write_frame(&mut write_half, STDOUT_DATA, &out[..n]).await?,
                _ => out_open = false,
            },
            read = child_stderr.read(&mut err), if err_open => match read {
                Ok(n) if n > 0 => write_frame(&mut write_half, STDERR_DATA, &err[..n]).await?,
                _ => err_open = false,
            },
        }
    }
    let code = child.wait().await.ok().and_then(|status| status.code());
    stdin_pump.abort();
    write_frame(&mut write_half, EXIT, &code.unwrap_or(255).to_be_bytes()).await
}

/// Run the client and return the process exit code.
pub fn run(args: &[String]) -> i32 {
    let [addr, token, ssh_args @ ..] = args else {
        eprintln!("{USAGE}");
        return 2;
    };
    let stream = match TcpStream::connect(addr) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("forge __ssh-broker: cannot reach the broker at {addr}: {error}");
            return 255;
        }
    };
    let git_protocol = std::env::var("GIT_PROTOCOL").unwrap_or_default();
    match client_session(
        stream,
        token,
        &git_protocol,
        ssh_args,
        io::stdin(),
        io::stdout(),
        io::stderr(),
    ) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("forge __ssh-broker: {error}");
            255
        }
    }
}

/// One ssh session through the broker: send the request, then carry `stdin`
/// to it and its output to `stdout` and `stderr`. Returns ssh's exit code.
fn client_session(
    stream: TcpStream,
    token: &str,
    git_protocol: &str,
    args: &[String],
    mut stdin: impl Read + Send + 'static,
    mut stdout: impl Write,
    mut stderr: impl Write,
) -> io::Result<i32> {
    if args.iter().any(|arg| arg.contains('\n')) || git_protocol.contains('\n') {
        return Err(io::Error::other("an argument contains a newline"));
    }
    let mut writer = stream.try_clone()?;
    let mut header = format!("{token}\n{git_protocol}\n{}\n", args.len());
    for arg in args {
        header.push_str(arg);
        header.push('\n');
    }
    writer.write_all(header.as_bytes())?;
    writer.flush()?;

    std::thread::spawn(move || {
        let mut buffer = vec![0; 32 * 1024];
        loop {
            match stdin.read(&mut buffer) {
                Ok(n) if n > 0 => {
                    let sent = writer
                        .write_all(&[STDIN_DATA])
                        .and_then(|()| writer.write_all(&(n as u32).to_be_bytes()))
                        .and_then(|()| writer.write_all(&buffer[..n]))
                        .and_then(|()| writer.flush());
                    if sent.is_err() {
                        return;
                    }
                }
                _ => {
                    let _ = writer
                        .write_all(&[STDIN_EOF])
                        .and_then(|()| writer.write_all(&0u32.to_be_bytes()))
                        .and_then(|()| writer.flush());
                    return;
                }
            }
        }
    });

    let mut reader = io::BufReader::new(stream);
    loop {
        let mut head = [0u8; 5];
        reader.read_exact(&mut head)?;
        let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
        if len > MAX_FRAME_BYTES {
            return Err(io::Error::other("oversized frame from the broker"));
        }
        let mut payload = vec![0; len];
        reader.read_exact(&mut payload)?;
        match head[0] {
            STDOUT_DATA => {
                stdout.write_all(&payload)?;
                stdout.flush()?;
            }
            STDERR_DATA => {
                stderr.write_all(&payload)?;
                stderr.flush()?;
            }
            EXIT => {
                let code = payload.get(..4).map_or(255, |bytes| {
                    i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
                });
                return Ok(code);
            }
            _ => return Err(io::Error::other("unknown frame from the broker")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn git_lfs_may_ask_for_a_token_and_an_upload_is_a_push() {
        let download = parse_request(&args(&[
            "git@github.com",
            "git-lfs-authenticate 'NorviaLabs/forge.git' download",
        ]))
        .unwrap();
        assert!(!download.is_push());
        let upload = parse_request(&args(&[
            "git@github.com",
            "git-lfs-authenticate NorviaLabs/forge.git upload",
        ]))
        .unwrap();
        assert!(upload.is_push());
        for command in [
            "git-lfs-authenticate repo.git delete",
            "git-lfs-authenticate repo.git;id download",
            "git-lfs-authenticate download",
            "git-lfs-authenticate 'a' 'b' download",
        ] {
            assert!(
                parse_request(&args(&["git@github.com", command])).is_err(),
                "{command}"
            );
        }
    }

    #[test]
    fn the_invocations_git_makes_are_accepted() {
        let request = parse_request(&args(&[
            "-o",
            "SendEnv=GIT_PROTOCOL",
            "-p",
            "2222",
            "git@github.com",
            "git-upload-pack 'NorviaLabs/forge.git'",
        ]))
        .unwrap();
        assert_eq!(request.host(), "github.com");
        assert_eq!(request.port, Some(2222));
        assert!(request.send_git_protocol && !request.is_push());

        let push = parse_request(&args(&[
            "example.test",
            "git-receive-pack '/srv/it'\\''s.git'",
        ]))
        .unwrap();
        assert!(push.is_push());
        assert_eq!(push.host(), "example.test");
    }

    /// The caller is a confined process and the command runs with the user's
    /// keys, so anything git itself does not send is refused.
    #[test]
    fn anything_else_is_refused() {
        for (words, why) in [
            (vec!["host", "sh -c id"], "an arbitrary remote command"),
            (
                vec!["host", "git-upload-pack 'a'; id"],
                "a second command after the path",
            ),
            (vec!["host", "git-upload-pack a"], "an unquoted path"),
            (vec!["host", "git-upload-pack 'a' 'b'"], "two paths"),
            (vec!["host", "git-upload-pack 'a"], "an unterminated quote"),
            (
                vec!["host", "git-upload-pack '$(id)'x"],
                "text after the quote",
            ),
            (
                vec!["-oProxyCommand=id", "host", "git-upload-pack 'a'"],
                "an ssh option",
            ),
            (
                vec!["-o", "ProxyCommand=id", "host", "git-upload-pack 'a'"],
                "an ssh option",
            ),
            (
                vec!["-F", "/tmp/config", "host", "git-upload-pack 'a'"],
                "a config file",
            ),
            (
                vec!["-i", "key", "host", "git-upload-pack 'a'"],
                "an identity file",
            ),
            (
                vec!["host name", "git-upload-pack 'a'"],
                "a destination with a space",
            ),
            (
                vec!["host", "git-upload-pack 'a'", "extra"],
                "a trailing word",
            ),
            (vec!["host"], "no command"),
            (vec![], "nothing"),
        ] {
            assert!(parse_request(&args(&words)).is_err(), "{why}: {words:?}");
        }
    }

    /// A fake ssh that reports the words it was given and echoes stdin, so
    /// the round trip through the broker is checked without a network.
    fn fake_ssh() -> (tempfile::TempDir, String) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("ssh");
        std::fs::write(
            &script,
            "#!/bin/sh\nfor word in \"$@\"; do printf '[%s]' \"$word\"; done\n\
             printf '\\n'\nprintf 'protocol=%s\\n' \"$GIT_PROTOCOL\" >&2\ncat\nexit 7\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let command = shell_quote(script.to_str().unwrap());
        (dir, command)
    }

    async fn session(
        broker: &SshBroker,
        token: &str,
        words: Vec<String>,
        input: &'static [u8],
    ) -> (i32, String, String) {
        let addr = broker.addr();
        let token = token.to_string();
        tokio::task::spawn_blocking(move || {
            let stream = TcpStream::connect(addr).unwrap();
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = client_session(
                stream,
                &token,
                "version=2",
                &words,
                input,
                &mut out,
                &mut err,
            )
            .unwrap();
            (
                code,
                String::from_utf8(out).unwrap(),
                String::from_utf8(err).unwrap(),
            )
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_git_request_round_trips_through_the_broker() {
        let (_dir, command) = fake_ssh();
        let broker = SshBroker::start_fixed(command, authorize_all())
            .await
            .unwrap();
        let token = broker.token.clone();
        let words = args(&[
            "-o",
            "SendEnv=GIT_PROTOCOL",
            "git@example.test",
            "git-upload-pack 'repo.git'",
        ]);

        let (code, out, err) = session(&broker, &token, words, b"pack data").await;
        assert_eq!(code, 7, "ssh's exit code comes back");
        assert_eq!(
            out,
            "[-o][BatchMode=yes][-o][SendEnv=GIT_PROTOCOL][--][git@example.test]\
             [git-upload-pack 'repo.git']\npack data"
        );
        assert_eq!(err, "protocol=version=2\n");
    }

    #[tokio::test]
    async fn a_wrong_token_an_unchecked_request_and_a_refusal_run_nothing() {
        let (_dir, command) = fake_ssh();
        let broker = SshBroker::start_fixed(
            command,
            Arc::new(|request: SshRequest| {
                Box::pin(async move {
                    if request.is_push() {
                        Err("pushing was not allowed".into())
                    } else {
                        Ok(())
                    }
                })
            }),
        )
        .await
        .unwrap();
        let token = broker.token.clone();
        let fetch = args(&["example.test", "git-upload-pack 'r'"]);

        let (code, out, err) = session(&broker, "wrong", fetch.clone(), b"").await;
        assert_eq!((code, out.as_str()), (255, ""));
        assert!(err.contains("refusing git over SSH"), "{err}");

        let (code, out, err) = session(&broker, &token, args(&["example.test", "id"]), b"").await;
        assert_eq!((code, out.as_str()), (255, ""));
        assert!(err.contains("not a git service"), "{err}");

        let push = args(&["example.test", "git-receive-pack 'r'"]);
        let (code, out, err) = session(&broker, &token, push, b"").await;
        assert_eq!((code, out.as_str()), (255, ""));
        assert!(err.contains("pushing was not allowed"), "{err}");

        let (code, _, _) = session(&broker, &token, fetch, b"").await;
        assert_eq!(code, 7, "a checked, authorised request still runs");
    }
}
