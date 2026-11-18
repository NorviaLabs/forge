//! Starting and stopping the agent's browser process.
//!
//! The browser runs with a fresh profile in the session's temp directory
//! (no cookies, logins or extensions of the user's), talks to Forge over the
//! debugging pipe only, and sends all of its traffic through the session's
//! egress gateway — loopback included, so `localhost` needs a grant too.
//! It runs in its own process group, which is killed when the browser is
//! closed or its handle dropped.

use std::net::SocketAddr;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::PathBuf;

use thiserror::Error;

use crate::cdp::CdpConnection;

/// The viewport the agent works in; screenshots are this size.
pub const WINDOW_WIDTH: u32 = 1280;
pub const WINDOW_HEIGHT: u32 = 800;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchOptions {
    pub executable: PathBuf,
    /// A fresh, private profile directory.
    pub profile_dir: PathBuf,
    /// The egress gateway every request goes through. `None` only for
    /// callers without a session gateway (tests).
    pub proxy: Option<SocketAddr>,
    /// The one origin reached directly, around the proxy: the app a web
    /// check opens. Everything else still meets the proxy.
    pub direct_origin: Option<SocketAddr>,
    pub headless: bool,
    /// Extra switches, appended after Forge's own.
    pub extra_args: Vec<String>,
}

/// The command line for `options`, without the program.
pub fn launch_args(options: &LaunchOptions) -> Vec<String> {
    let mut args = vec![
        "--remote-debugging-pipe".to_string(),
        format!("--user-data-dir={}", options.profile_dir.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        // A fresh profile must not ask for the keyring, sync, or phone home
        // on its own: background requests would only meet the gateway.
        "--password-store=basic".into(),
        "--use-mock-keychain".into(),
        "--disable-sync".into(),
        "--disable-background-networking".into(),
        "--disable-component-update".into(),
        "--disable-default-apps".into(),
        format!("--window-size={WINDOW_WIDTH},{WINDOW_HEIGHT}"),
    ];
    if let Some(proxy) = options.proxy {
        args.push(format!("--proxy-server=http://{proxy}"));
        // Chrome sends loopback direct by default; `<-loopback>` takes that
        // out, so `localhost` meets the gateway (and needs a grant) too.
        // A direct origin is the one exception, by exact address and port.
        match options.direct_origin {
            Some(origin) => args.push(format!("--proxy-bypass-list=<-loopback>;{origin}")),
            None => args.push("--proxy-bypass-list=<-loopback>".into()),
        }
        // The gateway resolves names; the browser resolves none itself (DNS
        // lookups are a channel out too), except the gateway's own address.
        args.push(format!(
            "--host-resolver-rules=MAP * ~NOTFOUND , EXCLUDE {}",
            proxy.ip()
        ));
    }
    // Nothing leaves around the gateway: WebRTC may not send UDP of its own
    // (STUN/TURN would reach any host), and no QUIC.
    args.push("--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into());
    args.push("--disable-quic".into());
    if options.headless {
        args.push("--headless=new".into());
    }
    args.extend(options.extra_args.iter().cloned());
    args.push("about:blank".into());
    args
}

#[derive(Debug, Error)]
pub enum LaunchError {
    #[error("could not start the browser `{path}`: {reason}")]
    Spawn { path: String, reason: String },
    #[error("the browser started but did not answer: {0}")]
    Handshake(String),
}

/// A running browser.
pub struct BrowserProcess {
    child: tokio::process::Child,
    pid: u32,
}

/// How long a starting browser has to answer its first call.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

impl BrowserProcess {
    /// Start the browser and connect to it over the debugging pipe.
    pub async fn launch(options: &LaunchOptions) -> Result<(Self, CdpConnection), LaunchError> {
        let spawn_error = |reason: String| LaunchError::Spawn {
            path: options.executable.display().to_string(),
            reason,
        };
        // Two pipes: the browser reads commands on fd 3, writes on fd 4.
        let (to_browser_read, to_browser_write) = pipe().map_err(|e| spawn_error(e.to_string()))?;
        let (from_browser_read, from_browser_write) =
            pipe().map_err(|e| spawn_error(e.to_string()))?;

        let mut command = tokio::process::Command::new(&options.executable);
        command
            .args(launch_args(options))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let child_read = to_browser_read.as_raw_fd();
        let child_write = from_browser_write.as_raw_fd();
        // SAFETY: only async-signal-safe calls (fcntl, dup2, close, setpgid)
        // run between fork and exec.
        unsafe {
            command.pre_exec(move || {
                // Move both ends above 3 and 4 first, so neither dup2 below
                // can overwrite the other end.
                let read = libc::fcntl(child_read, libc::F_DUPFD_CLOEXEC, 10);
                let write = libc::fcntl(child_write, libc::F_DUPFD_CLOEXEC, 10);
                if read < 0 || write < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // dup2 clears close-on-exec on the new descriptor.
                if libc::dup2(read, 3) < 0 || libc::dup2(write, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(read);
                libc::close(write);
                // Its own process group, so closing kills every helper.
                if libc::setpgid(0, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().map_err(|e| spawn_error(e.to_string()))?;
        // The child's ends now live in the child only.
        drop(to_browser_read);
        drop(from_browser_write);
        let pid = child.id().unwrap_or(0);
        let process = Self { child, pid };

        let writer =
            tokio::net::unix::pipe::Sender::from_file(std::fs::File::from(to_browser_write))
                .map_err(|e| spawn_error(e.to_string()))?;
        let reader =
            tokio::net::unix::pipe::Receiver::from_file(std::fs::File::from(from_browser_read))
                .map_err(|e| spawn_error(e.to_string()))?;
        let cdp = CdpConnection::spawn(reader, writer);
        let handshake = cdp
            .clone()
            .with_timeout(HANDSHAKE_TIMEOUT)
            .call("Browser.getVersion", serde_json::json!({}))
            .await;
        match handshake {
            Ok(_) => Ok((process, cdp)),
            Err(error) => {
                let mut process = process;
                process.kill().await;
                Err(LaunchError::Handshake(error.to_string()))
            }
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Kill the browser's process group and reap it.
    pub async fn kill(&mut self) {
        self.kill_group();
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }

    fn kill_group(&self) {
        if self.pid > 0 {
            // SAFETY: signalling a process group we created.
            unsafe {
                libc::kill(-(self.pid as libc::pid_t), libc::SIGKILL);
            }
        }
    }
}

impl Drop for BrowserProcess {
    fn drop(&mut self) {
        self.kill_group();
        // `kill_on_drop` kills and the runtime reaps the leader.
    }
}

/// A close-on-exec pipe: `(read, write)`.
fn pipe() -> std::io::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // Atomically close-on-exec where the platform can, so a command another
    // thread spawns at the same moment never inherits the browser's pipe.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    // SAFETY: `fds` has room for the two descriptors pipe2 writes.
    let created = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    // SAFETY: `fds` has room for the two descriptors pipe writes.
    let created = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if created < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both descriptors were just created and are owned here.
    let ends = unsafe {
        (
            std::os::fd::OwnedFd::from_raw_fd(fds[0]),
            std::os::fd::OwnedFd::from_raw_fd(fds[1]),
        )
    };
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    for fd in fds {
        // SAFETY: fd is a valid descriptor owned by `ends`.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(ends)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> LaunchOptions {
        LaunchOptions {
            executable: "/usr/bin/chromium".into(),
            profile_dir: "/tmp/forge-browser-profile".into(),
            proxy: Some("127.0.0.1:40123".parse().unwrap()),
            direct_origin: None,
            headless: false,
            extra_args: vec!["--lang=en-US".into()],
        }
    }

    #[test]
    fn the_browser_talks_over_the_pipe_with_a_private_profile() {
        let args = launch_args(&options());
        assert!(args.contains(&"--remote-debugging-pipe".to_string()));
        assert!(
            !args
                .iter()
                .any(|arg| arg.starts_with("--remote-debugging-port")),
            "no debugging port another process could attach to"
        );
        assert!(args.contains(&"--user-data-dir=/tmp/forge-browser-profile".to_string()));
        for switch in ["--no-first-run", "--no-default-browser-check"] {
            assert!(args.contains(&switch.to_string()), "{switch}");
        }
        assert!(args.contains(&format!("--window-size={WINDOW_WIDTH},{WINDOW_HEIGHT}")));
        assert!(
            !args.iter().any(|arg| arg == "--no-sandbox"),
            "the browser keeps its sandbox"
        );
        assert_eq!(args.last().map(String::as_str), Some("about:blank"));
    }

    /// Every request, loopback included, goes through the gateway.
    #[test]
    fn all_traffic_goes_through_the_gateway() {
        let args = launch_args(&options());
        assert!(args.contains(&"--proxy-server=http://127.0.0.1:40123".to_string()));
        assert!(args.contains(&"--proxy-bypass-list=<-loopback>".to_string()));

        let direct = launch_args(&LaunchOptions {
            proxy: None,
            ..options()
        });
        assert!(!direct.iter().any(|arg| arg.starts_with("--proxy")));
    }

    /// Nothing leaves around the gateway: no direct WebRTC UDP, no local
    /// DNS (the gateway resolves names), no QUIC.
    #[test]
    fn the_browser_has_no_way_around_the_gateway() {
        let args = launch_args(&options());
        for switch in [
            "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
            "--host-resolver-rules=MAP * ~NOTFOUND , EXCLUDE 127.0.0.1",
            "--disable-quic",
        ] {
            assert!(args.contains(&switch.to_string()), "{switch}: {args:?}");
        }
    }

    /// A web check reaches its app directly and nothing else: only that
    /// exact origin is taken out of the proxy.
    #[test]
    fn only_the_direct_origin_bypasses_the_proxy() {
        let args = launch_args(&LaunchOptions {
            direct_origin: Some("127.0.0.1:43817".parse().unwrap()),
            ..options()
        });
        assert!(
            args.contains(&"--proxy-bypass-list=<-loopback>;127.0.0.1:43817".to_string()),
            "{args:?}"
        );
        assert_eq!(
            args.iter()
                .filter(|arg| arg.starts_with("--proxy-bypass-list"))
                .count(),
            1
        );
        assert!(args.contains(&"--proxy-server=http://127.0.0.1:40123".to_string()));
        // Without a proxy there is nothing to bypass: no direct origin alone.
        let unproxied = launch_args(&LaunchOptions {
            proxy: None,
            direct_origin: Some("127.0.0.1:43817".parse().unwrap()),
            ..options()
        });
        assert!(!unproxied.iter().any(|arg| arg.starts_with("--proxy")));
    }

    #[test]
    fn headless_is_a_choice_and_extra_args_come_last() {
        assert!(!launch_args(&options())
            .iter()
            .any(|arg| arg.starts_with("--headless")));
        let headless = launch_args(&LaunchOptions {
            headless: true,
            ..options()
        });
        assert!(headless.contains(&"--headless=new".to_string()));
        let position = |args: &[String], value: &str| args.iter().position(|a| a == value).unwrap();
        assert!(
            position(&headless, "--lang=en-US") > position(&headless, "--no-first-run"),
            "the user's extra switches follow Forge's"
        );
    }

    #[tokio::test]
    async fn a_missing_program_fails_to_launch() {
        let error = BrowserProcess::launch(&LaunchOptions {
            executable: "/nowhere/chromium".into(),
            ..options()
        })
        .await
        .err()
        .expect("cannot start");
        assert!(matches!(error, LaunchError::Spawn { .. }), "{error}");
    }
}
