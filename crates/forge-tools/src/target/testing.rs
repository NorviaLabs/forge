//! Test support (feature `test-support`): a real `sshd`, run as the current
//! user on a loopback port behind a relay the test can freeze.
//!
//! The server sets `FORGE_TEST_REMOTE=1` for every session, so a test can
//! prove a command ran over SSH and not on this machine. It accepts any
//! environment a client sends (`AcceptEnv *`), so a test can prove Forge
//! sends none. Its workspace (the
//! *remote root*) is a directory of its own, apart from any local workspace.
//!
//! [`SshServer::start`] returns `None` (and says so on stderr) when OpenSSH
//! is not installed, so the suite skips rather than fails on such a machine.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use super::{SshOptions, SshTarget};

/// The alias of the working test host in [`SshServer::config`].
pub const HOST: &str = "forge-test";
/// A host whose key is not in `known_hosts`.
pub const UNKNOWN_KEY_HOST: &str = "forge-test-unknown-key";
/// A host nothing listens for.
pub const DOWN_HOST: &str = "forge-test-down";

/// `name` on `PATH`, or in the `sbin` directories `sshd` lives in and a
/// user's `PATH` often leaves out.
fn find_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(["/usr/sbin", "/usr/local/sbin", "/sbin"].map(PathBuf::from))
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|addr| addr.port())
        .expect("a free loopback port")
}

fn user_name() -> String {
    std::env::var("USER")
        .ok()
        .filter(|user| !user.is_empty())
        .unwrap_or_else(|| {
            let out = Command::new("id").arg("-un").output().expect("id runs");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        })
}

/// Extra server settings.
#[derive(Debug, Default, Clone)]
pub struct ServerOptions {
    /// Directories put before the default `PATH` of remote sessions (to offer
    /// `rg`, or to hide a tool).
    pub path_prefix: Vec<PathBuf>,
    /// Use `path_prefix` as the whole `PATH`, without the default directories.
    pub only_path: bool,
    /// Delay added to each hop of the relay, each way: a network with a
    /// round trip of about twice this.
    pub latency: Duration,
    /// sshd's `MaxSessions` (sessions per connection); `None` is 64.
    pub max_sessions: Option<u32>,
}

/// A running test server. Dropping it stops `sshd` and the relay.
pub struct SshServer {
    dir: tempfile::TempDir,
    remote: tempfile::TempDir,
    sshd: Child,
    frozen: Arc<AtomicBool>,
    /// Cancelled to close the connections relayed so far; replaced by a
    /// fresh token so later connections still go through.
    sever: Arc<std::sync::Mutex<CancellationToken>>,
    stop: CancellationToken,
    relay: tokio::task::JoinHandle<()>,
}

impl SshServer {
    pub async fn start() -> Option<Self> {
        Self::start_with(ServerOptions::default()).await
    }

    pub async fn start_with(options: ServerOptions) -> Option<Self> {
        let (Some(sshd), Some(keygen), Some(_ssh)) = (
            find_program("sshd"),
            find_program("ssh-keygen"),
            find_program("ssh"),
        ) else {
            eprintln!("SKIPPED: OpenSSH (ssh, sshd, ssh-keygen) is not on PATH");
            return None;
        };
        let dir = tempfile::tempdir().expect("a temp dir");
        let remote = tempfile::tempdir().expect("a remote root");
        let d = dir.path();
        for key in ["host_key", "client_key"] {
            let status = Command::new(&keygen)
                .args(["-q", "-t", "ed25519", "-N", ""])
                .arg("-f")
                .arg(d.join(key))
                .status()
                .expect("ssh-keygen runs");
            assert!(status.success(), "ssh-keygen {key}");
        }
        std::fs::copy(d.join("client_key.pub"), d.join("authorized_keys")).unwrap();

        let sshd_port = free_port();
        let mut setenv = String::from("SetEnv FORGE_TEST_REMOTE=1");
        if !options.path_prefix.is_empty() {
            let prefix: Vec<String> = options
                .path_prefix
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            let rest = if options.only_path {
                ""
            } else {
                ":/usr/local/bin:/usr/bin:/bin"
            };
            setenv.push_str(&format!(" PATH={}{rest}", prefix.join(":")));
        }
        std::fs::write(
            d.join("sshd_config"),
            format!(
                "Port {sshd_port}\nListenAddress 127.0.0.1\nHostKey {host}\nPidFile {pid}\n\
                 AuthorizedKeysFile {auth}\nPasswordAuthentication no\n\
                 KbdInteractiveAuthentication no\nUsePAM no\nStrictModes no\n\
                 MaxSessions {max_sessions}\nMaxStartups 64\nAcceptEnv *\n{setenv}\n",
                host = d.join("host_key").display(),
                pid = d.join("sshd.pid").display(),
                auth = d.join("authorized_keys").display(),
                max_sessions = options.max_sessions.unwrap_or(64),
            ),
        )
        .unwrap();
        let child = Command::new(&sshd)
            .arg("-D")
            .arg("-f")
            .arg(d.join("sshd_config"))
            .arg("-E")
            .arg(d.join("sshd.log"))
            .stdin(Stdio::null())
            .spawn()
            .expect("sshd starts");

        // The relay: ssh → relay → sshd, so a test can freeze the link.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay_port = listener.local_addr().unwrap().port();
        let frozen = Arc::new(AtomicBool::new(false));
        let sever = Arc::new(std::sync::Mutex::new(CancellationToken::new()));
        let stop = CancellationToken::new();
        let relay = tokio::spawn(relay(
            listener,
            SocketAddr::from(([127, 0, 0, 1], sshd_port)),
            frozen.clone(),
            sever.clone(),
            stop.clone(),
            options.latency,
        ));

        let host_key = std::fs::read_to_string(d.join("host_key.pub")).unwrap();
        std::fs::write(
            d.join("known_hosts"),
            format!("[127.0.0.1]:{relay_port} {host_key}"),
        )
        .unwrap();
        std::fs::write(d.join("empty_known_hosts"), "").unwrap();
        let user = user_name();
        let common = |port: u16, known: &str| {
            format!(
                "  HostName 127.0.0.1\n  Port {port}\n  User {user}\n  IdentityFile {key}\n\
                 \x20 IdentitiesOnly yes\n  UserKnownHostsFile {known}\n\
                 \x20 StrictHostKeyChecking yes\n  LogLevel ERROR\n",
                key = d.join("client_key").display(),
                known = d.join(known).display(),
            )
        };
        std::fs::write(
            d.join("ssh_config"),
            format!(
                "Host {HOST}\n{ok}  ForwardAgent yes\n  ForwardX11 yes\n  SendEnv *\n\n\
                 Host {UNKNOWN_KEY_HOST}\n{unknown}\n\
                 Host {DOWN_HOST}\n{down}",
                ok = common(relay_port, "known_hosts"),
                unknown = common(relay_port, "empty_known_hosts"),
                down = common(free_port(), "known_hosts"),
            ),
        )
        .unwrap();

        let server = Self {
            dir,
            remote,
            sshd: child,
            frozen,
            sever,
            stop,
            relay,
        };
        server.wait_until_accepting(sshd_port).await;
        Some(server)
    }

    async fn wait_until_accepting(&self, port: u16) {
        for _ in 0..100 {
            if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!(
            "sshd did not start: {}",
            std::fs::read_to_string(self.dir.path().join("sshd.log")).unwrap_or_default()
        );
    }

    /// The test `ssh` config (`ssh -F`).
    pub fn config(&self) -> PathBuf {
        self.dir.path().join("ssh_config")
    }

    /// The remote workspace, canonical.
    pub fn remote_root(&self) -> PathBuf {
        self.remote.path().canonicalize().unwrap()
    }

    /// Options for a target on this server: its config, and a keepalive of
    /// one second twice, so a dead link shows within about three seconds.
    pub fn options(&self) -> SshOptions {
        SshOptions {
            config_file: Some(self.config()),
            connect_timeout: Duration::from_secs(5),
            keepalive_interval: Duration::from_secs(1),
            keepalive_count: 2,
            ..SshOptions::default()
        }
    }

    /// [`Self::options`], with `ssh` replaced by a wrapper that appends each
    /// invocation's arguments to `log` (one line each) before running it:
    /// how a test counts the round trips a tool makes.
    pub fn counting_options(&self, log: &Path) -> SshOptions {
        let ssh = find_program("ssh").expect("ssh");
        let wrapper = self.dir.path().join("counting-ssh");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" | tr '\\n' ' ' >> '{log}'\necho >> '{log}'\nexec '{ssh}' \"$@\"\n",
                log = log.display(),
                ssh = ssh.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        SshOptions {
            program: wrapper,
            ..self.options()
        }
    }

    /// A target for [`HOST`] and the remote root, with its control socket in
    /// `local_dir`.
    pub fn target(&self, local_dir: &Path) -> SshTarget {
        SshTarget::new(
            HOST,
            &self.remote_root().display().to_string(),
            local_dir,
            self.options(),
        )
    }

    /// Whether this server can give a session a terminal. An `sshd` that is
    /// not root cannot hand a terminal to the user it runs as on most
    /// systems, and drops such a session.
    pub async fn allocates_terminals(&self) -> bool {
        let out = tokio::process::Command::new("ssh")
            .arg("-F")
            .arg(self.config())
            .args(["-tt", "-o", "BatchMode=yes", "--", HOST, "tty"])
            .stdin(Stdio::null())
            .output()
            .await;
        matches!(out, Ok(out) if out.status.success()
            && String::from_utf8_lossy(&out.stdout).contains("/dev/"))
    }

    /// Stop relaying bytes, keeping the sockets open: a network that went
    /// silent, the case that hangs without keepalives.
    pub fn freeze(&self) {
        self.frozen.store(true, Ordering::SeqCst);
    }

    pub fn thaw(&self) {
        self.frozen.store(false, Ordering::SeqCst);
    }

    /// Close every connection relayed so far. New ones still go through.
    pub fn sever(&self) {
        let mut current = self.sever.lock().unwrap();
        current.cancel();
        *current = CancellationToken::new();
    }
}

impl Drop for SshServer {
    fn drop(&mut self) {
        self.sever.lock().unwrap().cancel();
        self.stop.cancel();
        self.relay.abort();
        let _ = self.sshd.kill();
        let _ = self.sshd.wait();
    }
}

async fn relay(
    listener: TcpListener,
    upstream: SocketAddr,
    frozen: Arc<AtomicBool>,
    sever: Arc<std::sync::Mutex<CancellationToken>>,
    stop: CancellationToken,
    latency: Duration,
) {
    loop {
        let accepted = tokio::select! {
            () = stop.cancelled() => return,
            accepted = listener.accept() => accepted,
        };
        let Ok((client, _)) = accepted else { continue };
        let frozen = frozen.clone();
        let sever = sever.lock().unwrap().clone();
        tokio::spawn(async move {
            let Ok(server) = TcpStream::connect(upstream).await else {
                return;
            };
            // The relay forwards as it receives: it must not add a delay of
            // its own (Nagle) on top of the one it is asked for.
            let _ = client.set_nodelay(true);
            let _ = server.set_nodelay(true);
            let (cr, cw) = client.into_split();
            let (sr, sw) = server.into_split();
            let up = pump(cr, sw, frozen.clone(), latency);
            let down = pump(sr, cw, frozen, latency);
            tokio::select! {
                () = sever.cancelled() => {}
                _ = async { tokio::join!(up, down) } => {}
            }
        });
    }
}

async fn pump(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    frozen: Arc<AtomicBool>,
    latency: Duration,
) {
    // Bytes are held for `latency` from when they arrived, not one chunk
    // after another: a link with that delay still carries many packets at
    // once.
    let (sender, mut receiver) =
        tokio::sync::mpsc::unbounded_channel::<(tokio::time::Instant, Vec<u8>)>();
    let forward = async move {
        while let Some((arrived, bytes)) = receiver.recv().await {
            tokio::time::sleep_until(arrived + latency).await;
            // Frozen: hold the bytes and the sockets, as a silent network does.
            while frozen.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            if to.write_all(&bytes).await.is_err() {
                return;
            }
        }
        let _ = to.shutdown().await;
    };
    let read = async move {
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let Ok(count) = from.read(&mut buffer).await else {
                return;
            };
            if count == 0 {
                return;
            }
            if sender
                .send((tokio::time::Instant::now(), buffer[..count].to_vec()))
                .is_err()
            {
                return;
            }
        }
    };
    tokio::join!(read, forward);
}
