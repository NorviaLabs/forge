//! The host side of a sandboxed app's ingress: a loopback TCP port that
//! carries each connection to the app's socket.
//!
//! The app runs under `--unshare-net`, so the host cannot reach its port.
//! Inside the sandbox a relay listens on a socket in a directory the sandbox
//! can write (see `SandboxPolicy::with_ingress`). Here Forge listens on a
//! random `127.0.0.1` port while the app runs and joins each connection to
//! that socket.
//!
//! The socket's directory is writable from inside, so the file at the path
//! is whatever the sandbox last put there. It is opened without following a
//! link, checked to be a socket of this user's, and connected to through the
//! opened descriptor: a sandbox that swaps it for a link to a host socket
//! (Docker's, D-Bus) reaches nothing.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Most connections carried at once; more are closed at once.
pub const MAX_CONNECTIONS: usize = 64;

/// A running ingress listener. Dropping it stops listening and closes every
/// connection it carries.
pub struct IngressRelay {
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl IngressRelay {
    /// Listen on a random loopback port and carry connections to `socket`.
    /// Only this user's connections are carried.
    pub async fn start(socket: PathBuf) -> std::io::Result<Self> {
        Self::start_for(socket, current_uid()).await
    }

    /// As [`Self::start`], carrying only connections owned by `uid`.
    async fn start_for(socket: PathBuf, uid: u32) -> std::io::Result<Self> {
        Self::start_with(socket, uid, connection_owner).await
    }

    /// As [`Self::start_for`], telling owners with `owner_of` (tests slow
    /// it down).
    async fn start_with(
        socket: PathBuf,
        uid: u32,
        owner_of: fn(SocketAddr, SocketAddr) -> Option<u32>,
    ) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
        let task = tokio::spawn(async move {
            // Owned here: aborting this task drops the set, which aborts
            // every connection with it.
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (stream, peer) = match accepted {
                            Ok(accepted) => accepted,
                            // Out of descriptors, or the like: back off.
                            Err(_) => {
                                tokio::time::sleep(Duration::from_millis(50)).await;
                                continue;
                            }
                        };
                        let Ok(permit) = permits.clone().try_acquire_owned() else {
                            continue; // dropped: closed at once
                        };
                        let socket = socket.clone();
                        connections.spawn(async move {
                            let _permit = permit;
                            // Another user on this machine gets nothing: the
                            // app is this user's. Unknown owners too.
                            // Reading the table can take a while on a busy
                            // machine: off the async workers.
                            let owner = tokio::task::spawn_blocking(move || owner_of(peer, addr))
                                .await
                                .ok()
                                .flatten();
                            if owner != Some(uid) {
                                return;
                            }
                            carry(stream, &socket).await;
                        });
                    }
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Ok(Self { addr, task })
    }

    /// Where the host reaches the app.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for IngressRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// This process's effective user.
fn current_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// The user owning the client end of a loopback TCP connection from `peer`
/// to `local`, from `/proc/net/tcp`. `None` when it cannot be told.
fn connection_owner(peer: SocketAddr, local: SocketAddr) -> Option<u32> {
    let hex = |addr: SocketAddr| match addr {
        SocketAddr::V4(addr) => Some(format!(
            "{:08X}:{:04X}",
            u32::from_le_bytes(addr.ip().octets()),
            addr.port()
        )),
        SocketAddr::V6(_) => None,
    };
    let table = std::fs::read_to_string("/proc/net/tcp").ok()?;
    owner_in(&table, &hex(peer)?, &hex(local)?)
}

/// The uid on the `/proc/net/tcp` line for a socket bound to `local_hex`
/// and connected to `remote_hex` (both `AABBCCDD:PORT`).
fn owner_in(table: &str, local_hex: &str, remote_hex: &str) -> Option<u32> {
    table.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let matches = fields.get(1)?.eq_ignore_ascii_case(local_hex)
            && fields.get(2)?.eq_ignore_ascii_case(remote_hex);
        matches.then(|| fields.get(7)?.parse().ok()).flatten()
    })
}

/// Join one host connection to the app's socket until either side closes.
async fn carry(mut tcp: tokio::net::TcpStream, socket: &Path) {
    let Ok(mut app) = connect_app_socket_async(socket).await else {
        return;
    };
    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut app).await;
}

/// The socket at `path`, opened without following a link, when it is a
/// socket of this user's. The descriptor names the inode that was checked.
#[cfg(target_os = "linux")]
fn open_app_socket(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    if !meta.file_type().is_socket() || meta.uid() != uid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{} is not the app's socket", path.display()),
        ));
    }
    Ok(file)
}

/// The path that connects to exactly the opened inode.
#[cfg(target_os = "linux")]
fn descriptor_path(file: &std::fs::File) -> String {
    use std::os::fd::AsRawFd;
    format!("/proc/self/fd/{}", file.as_raw_fd())
}

/// Connect to the app's socket at `path`, refusing anything but a socket of
/// this user's at exactly that path (no link is followed).
#[cfg(target_os = "linux")]
pub fn connect_app_socket(path: &Path) -> std::io::Result<std::os::unix::net::UnixStream> {
    let file = open_app_socket(path)?;
    std::os::unix::net::UnixStream::connect(descriptor_path(&file))
}

#[cfg(target_os = "linux")]
async fn connect_app_socket_async(path: &Path) -> std::io::Result<tokio::net::UnixStream> {
    let file = open_app_socket(path)?;
    // `file` stays open until the connect is done.
    tokio::net::UnixStream::connect(descriptor_path(&file)).await
}

/// Only Linux offers an app through a socket (see `SandboxPolicy::with_ingress`).
#[cfg(not(target_os = "linux"))]
pub fn connect_app_socket(path: &Path) -> std::io::Result<std::os::unix::net::UnixStream> {
    let _ = path;
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "an app socket is Linux only",
    ))
}

#[cfg(not(target_os = "linux"))]
async fn connect_app_socket_async(path: &Path) -> std::io::Result<tokio::net::UnixStream> {
    let _ = path;
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "an app socket is Linux only",
    ))
}

/// The HTTP status `addr` answers `GET path` with, within `timeout`.
pub async fn http_status(addr: SocketAddr, path: &str, timeout: Duration) -> std::io::Result<u16> {
    if !path.starts_with('/') || path.chars().any(|c| c.is_control() || c == ' ') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a request path: {path:?}"),
        ));
    }
    let exchange = async {
        let mut stream = tokio::net::TcpStream::connect(addr).await?;
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\nUser-Agent: forge-web-check\r\n\
             Accept: */*\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await?;
        let mut head = Vec::new();
        let mut buf = [0u8; 256];
        while !head.windows(2).any(|pair| pair == b"\r\n") && head.len() < 1024 {
            let read = stream.read(&mut buf).await?;
            if read == 0 {
                break;
            }
            head.extend_from_slice(&buf[..read]);
        }
        parse_status(&head).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "not an HTTP answer")
        })
    };
    tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "no answer in time"))?
}

/// `HTTP/1.x NNN ...` → `NNN`.
fn parse_status(head: &[u8]) -> Option<u16> {
    let line = head.split(|&b| b == b'\r').next()?;
    let line = std::str::from_utf8(line).ok()?;
    let mut parts = line.split(' ');
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    let code = parts.next()?;
    (code.len() == 3)
        .then(|| code.parse::<u16>().ok())
        .flatten()
        .filter(|code| (100..600).contains(code))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream, UnixListener};

    /// A Unix-socket server that answers each connection with `reply` after
    /// reading `expect` bytes, counting connections.
    fn serve(
        listener: UnixListener,
        expect: usize,
        reply: &'static [u8],
    ) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = vec![0; expect];
                    if stream.read_exact(&mut buf).await.is_ok() {
                        let _ = stream.write_all(reply).await;
                    }
                });
            }
        });
        count
    }

    async fn read_all(stream: &mut TcpStream) -> Vec<u8> {
        let mut out = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut out)).await;
        out
    }

    #[tokio::test]
    async fn a_connection_reaches_the_app_socket_and_back() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("app.sock");
        serve(UnixListener::bind(&socket).unwrap(), 4, b"pong");
        let relay = IngressRelay::start(socket).await.unwrap();
        assert!(relay.addr().ip().is_loopback());

        let mut client = TcpStream::connect(relay.addr()).await.unwrap();
        client.write_all(b"ping").await.unwrap();
        let mut reply = [0; 4];
        tokio::time::timeout(Duration::from_secs(5), client.read_exact(&mut reply))
            .await
            .expect("an answer")
            .unwrap();
        assert_eq!(&reply, b"pong");
    }

    /// Both directions carry a large body whole, and a close on one side
    /// reaches the other.
    #[tokio::test]
    async fn a_large_body_is_carried_whole_both_ways() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("app.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut read, mut write) = stream.into_split();
            tokio::io::copy(&mut read, &mut write).await.unwrap();
            write.shutdown().await.unwrap();
        });
        let relay = IngressRelay::start(socket).await.unwrap();
        let client = TcpStream::connect(relay.addr()).await.unwrap();
        let (mut read, mut write) = client.into_split();
        let body: Vec<u8> = (0..1_048_576u32).map(|i| (i % 251) as u8).collect();
        let sent = body.clone();
        let writer = tokio::spawn(async move {
            write.write_all(&sent).await.unwrap();
            write.shutdown().await.unwrap();
        });
        let mut echoed = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), read.read_to_end(&mut echoed))
            .await
            .expect("the echo ends")
            .unwrap();
        writer.await.unwrap();
        assert_eq!(echoed.len(), body.len());
        assert!(echoed == body);
    }

    /// The sandbox can replace the socket with a link to a host socket:
    /// the link is not followed, and the host socket sees nothing.
    #[tokio::test]
    async fn a_link_in_place_of_the_socket_reaches_nothing() {
        let host = tempfile::tempdir().unwrap();
        let host_socket = host.path().join("docker.sock");
        let reached = serve(UnixListener::bind(&host_socket).unwrap(), 1, b"!");
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("app.sock");
        std::os::unix::fs::symlink(&host_socket, &socket).unwrap();

        assert!(connect_app_socket(&socket).is_err());
        let relay = IngressRelay::start(socket).await.unwrap();
        let mut client = TcpStream::connect(relay.addr()).await.unwrap();
        let _ = client.write_all(b"x").await;
        assert!(read_all(&mut client).await.is_empty());
        assert_eq!(reached.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn only_a_socket_is_connected_to() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("app.sock");
        std::fs::write(&file, "not a socket").unwrap();
        assert!(connect_app_socket(&file).is_err());
        assert!(connect_app_socket(&dir.path().join("missing.sock")).is_err());
        let _ = std::fs::remove_file(&file);
        let _listener = std::os::unix::net::UnixListener::bind(&file).unwrap();
        assert!(connect_app_socket(&file).is_ok());
    }

    /// Nothing listens on the port once the relay is gone.
    #[tokio::test]
    async fn dropping_the_relay_stops_listening() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("app.sock");
        serve(UnixListener::bind(&socket).unwrap(), 1, b"!");
        let relay = IngressRelay::start(socket).await.unwrap();
        let addr = relay.addr();
        drop(relay);
        let mut refused = false;
        for _ in 0..50 {
            if TcpStream::connect(addr).await.is_err() {
                refused = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(refused, "the port must close with the relay");
    }

    /// Connections past the cap are closed at once; the ones carried keep
    /// working.
    #[tokio::test]
    async fn connections_past_the_cap_are_closed() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("app.sock");
        // Reads a byte, then holds the connection open without answering.
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });
        let relay = IngressRelay::start(socket).await.unwrap();
        let mut open = Vec::new();
        for _ in 0..MAX_CONNECTIONS {
            let mut client = TcpStream::connect(relay.addr()).await.unwrap();
            client.write_all(b"x").await.unwrap();
            open.push(client);
        }
        // Let the relay take them all before the one too many.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut extra = TcpStream::connect(relay.addr()).await.unwrap();
        let mut buf = [0; 1];
        let closed = tokio::time::timeout(Duration::from_secs(2), extra.read(&mut buf)).await;
        assert!(
            matches!(closed, Ok(Ok(0)) | Ok(Err(_))),
            "the connection past the cap must be closed: {closed:?}"
        );
        let mut first = open.remove(0);
        let still_open =
            tokio::time::timeout(Duration::from_millis(200), first.read(&mut buf)).await;
        assert!(still_open.is_err(), "a carried connection stays open");
    }

    /// Another user on this machine cannot reach the app: their connection
    /// is closed before it touches the socket.
    #[tokio::test]
    async fn another_users_connection_is_closed() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("app.sock");
        let reached = serve(UnixListener::bind(&socket).unwrap(), 1, b"!");
        let relay = IngressRelay::start_for(socket, current_uid() + 1)
            .await
            .unwrap();
        let mut client = TcpStream::connect(relay.addr()).await.unwrap();
        let _ = client.write_all(b"x").await;
        assert!(read_all(&mut client).await.is_empty());
        assert_eq!(reached.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// Telling a connection's owner reads a table that is large on a busy
    /// machine: it must not hold up the runtime's other work meanwhile.
    #[test]
    fn telling_the_owner_does_not_stall_the_runtime() {
        fn slow_owner(_: SocketAddr, _: SocketAddr) -> Option<u32> {
            std::thread::sleep(Duration::from_millis(300));
            Some(current_uid())
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("app.sock");
            serve(UnixListener::bind(&socket).unwrap(), 4, b"pong");
            let relay = IngressRelay::start_with(socket, current_uid(), slow_owner)
                .await
                .unwrap();
            let ticker = tokio::spawn(async {
                let mut longest = Duration::ZERO;
                let mut last = std::time::Instant::now();
                for _ in 0..40 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    longest = longest.max(last.elapsed());
                    last = std::time::Instant::now();
                }
                longest
            });
            let mut client = TcpStream::connect(relay.addr()).await.unwrap();
            client.write_all(b"ping").await.unwrap();
            let mut reply = [0; 4];
            client.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply, b"pong");
            let longest = ticker.await.unwrap();
            assert!(
                longest < Duration::from_millis(150),
                "other work stalled for {longest:?}"
            );
        });
    }

    /// The owner of a real loopback connection is found, as this user.
    #[tokio::test]
    async fn the_owner_of_a_local_connection_is_found() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = listener.local_addr().unwrap();
        let client = TcpStream::connect(local).await.unwrap();
        let (_server, peer) = listener.accept().await.unwrap();
        assert_eq!(peer, client.local_addr().unwrap());
        assert_eq!(connection_owner(peer, local), Some(current_uid()));
        let stranger: SocketAddr = "127.0.0.1:1".parse().unwrap();
        assert_eq!(connection_owner(stranger, local), None);
    }

    #[test]
    fn the_owner_is_read_from_the_tcp_table() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:A3F1 0100007F:1F90 01 00000000:00000000 00:00000000 00000000  1000        0 123 1 0 20 4 30 10 -1\n\
   1: 0100007F:1F90 0100007F:A3F1 01 00000000:00000000 00:00000000 00000000  1001        0 124 1 0 20 4 30 10 -1\n";
        assert_eq!(
            owner_in(table, "0100007F:A3F1", "0100007F:1F90"),
            Some(1000)
        );
        assert_eq!(
            owner_in(table, "0100007F:1F90", "0100007F:A3F1"),
            Some(1001)
        );
        assert_eq!(owner_in(table, "0100007F:0001", "0100007F:1F90"), None);
        assert_eq!(owner_in("", "0100007F:A3F1", "0100007F:1F90"), None);
    }

    async fn http_server(response: &'static [u8]) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0; 1024];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream.write_all(response).await;
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn the_status_of_an_answer_is_read() {
        let ok = http_server(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;
        assert_eq!(
            http_status(ok, "/", Duration::from_secs(2)).await.unwrap(),
            200
        );
        let missing = http_server(b"HTTP/1.0 404 Not Found\r\n\r\n").await;
        assert_eq!(
            http_status(missing, "/x", Duration::from_secs(2))
                .await
                .unwrap(),
            404
        );
    }

    #[tokio::test]
    async fn no_answer_is_an_error_within_the_timeout() {
        let closed = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap()
        };
        assert!(http_status(closed, "/", Duration::from_secs(2))
            .await
            .is_err());

        // Accepts and says nothing.
        let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = silent.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = silent.accept().await {
                held.push(stream);
            }
        });
        let started = std::time::Instant::now();
        assert!(http_status(addr, "/", Duration::from_millis(300))
            .await
            .is_err());
        assert!(started.elapsed() < Duration::from_secs(2));

        let garbage = http_server(b"SSH-2.0-OpenSSH\r\n").await;
        assert!(http_status(garbage, "/", Duration::from_secs(2))
            .await
            .is_err());
    }
}
