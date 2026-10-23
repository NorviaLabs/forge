//! The `git` tool's network traffic, end to end through the real tunnels.
//!
//! `GitTool` points git:// at a `GIT_PROXY_COMMAND` wrapper and SSH at a
//! `ProxyCommand`, both of which run `forge __egress-connect`. These tests run
//! the tool against loopback servers with the real `forge` binary as that
//! helper, so a byte only reaches the server if it went through the call's
//! egress gateway. Everything stays on 127.0.0.1.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_tools::egress::{EgressOptions, EgressPolicy, EgressProxy};
use forge_tools::sandbox::EgressGrant;
use forge_tools::{GitTool, Tool, ToolContext, ToolError};
use serde_json::json;
use tempfile::TempDir;

const CALL_ID: &str = "call-git-tunnel";

/// The `forge` binary under test. A runtime `CARGO_BIN_EXE_forge` wins over
/// the build-time path, so a copied test binary can be pointed at a copied
/// `forge`.
fn use_real_forge() {
    let forge = std::env::var_os("CARGO_BIN_EXE_forge")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_forge")));
    forge_tools::set_egress_connect_exe(forge);
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.email=forge@test",
            "-c",
            "user.name=Forge Test",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `<base>/repo.git`, a bare repository with one commit; returns its head.
fn bare_repo(base: &Path) -> String {
    let work = base.join("work");
    std::fs::create_dir(&work).unwrap();
    git(&work, &["init", "-q"]);
    std::fs::write(work.join("README"), "tunnelled\n").unwrap();
    git(&work, &["add", "README"]);
    git(&work, &["commit", "-q", "-m", "init"]);
    git(base, &["clone", "-q", "--bare", "work", "repo.git"]);
    git(&work, &["rev-parse", "HEAD"])
}

/// An empty workspace repository for the tool to fetch into.
fn workspace() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    dir
}

/// A tool context whose session policy allows exactly `allow`, with a live
/// gateway behind it, attributed to [`CALL_ID`].
async fn ctx(root: &Path, allow: &str, allow_private: bool) -> (EgressProxy, ToolContext) {
    let mut policy = EgressPolicy::new();
    policy.allow(allow);
    let proxy = EgressProxy::start_with_options(
        policy,
        EgressOptions {
            allow_private_destinations: allow_private,
            ..EgressOptions::default()
        },
    )
    .await
    .unwrap();
    let mut ctx = ToolContext::new(root.to_path_buf()).with_tool_call_id(CALL_ID);
    ctx.egress = Some(Arc::new(EgressGrant {
        proxy_port: proxy.addr().port(),
        socket_path: PathBuf::new(),
        control: Some(proxy.shared().clone()),
    }));
    (proxy, ctx)
}

async fn fetch(ctx: &ToolContext, url: &str) -> Result<forge_types::ToolOutput, ToolError> {
    GitTool
        .call(ctx, json!({"subcommand": "fetch", "args": [url]}))
        .await
}

/// Exactly one tunnel, to `127.0.0.1:port`, attributed to this call.
fn assert_one_tunnel(proxy: &EgressProxy, port: u16) {
    let events = proxy.shared().drain_events();
    let tunnels: Vec<_> = events.iter().filter(|e| e.port == port).collect();
    assert_eq!(tunnels.len(), 1, "{events:?}");
    let event = tunnels[0];
    assert!(event.allowed(), "{event:?}");
    assert_eq!(event.host, "127.0.0.1");
    assert_eq!(event.label.as_deref(), Some(CALL_ID));
    assert!(event.bytes_down > 0, "{event:?}");
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `git daemon` serving `base` on a loopback port, or `None` (with a message)
/// when this git has no daemon.
fn git_daemon(base: &Path) -> Option<(KillOnDrop, u16)> {
    let port = free_port();
    let child = Command::new("git")
        .arg("daemon")
        .arg("--export-all")
        .arg("--reuseaddr")
        .arg(format!("--base-path={}", base.display()))
        .arg(format!("--port={port}"))
        .arg("--listen=127.0.0.1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut daemon = match child {
        Ok(child) => KillOnDrop(child),
        Err(e) => {
            eprintln!("skipping: cannot run `git daemon`: {e}");
            return None;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(Some(status)) = daemon.0.try_wait() {
            eprintln!("skipping: `git daemon` exited with {status}");
            return None;
        }
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Some((daemon, port));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!("skipping: `git daemon` did not start listening");
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_protocol_fetch_runs_through_the_gateway() {
    use_real_forge();
    let base = tempfile::tempdir().unwrap();
    let head = bare_repo(base.path());
    let Some((_daemon, port)) = git_daemon(base.path()) else {
        return;
    };
    let ws = workspace();
    let (proxy, ctx) = ctx(ws.path(), "127.0.0.1", true).await;
    let _ = proxy.shared().drain_events(); // the daemon readiness probe was direct

    let out = fetch(&ctx, &format!("git://127.0.0.1:{port}/repo.git"))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(git(ws.path(), &["rev-parse", "FETCH_HEAD"]), head);
    assert_one_tunnel(&proxy, port);
}

/// A host outside the policy is refused before git runs, as the host-grant
/// prompt's structured denial.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_protocol_fetch_of_an_unpermitted_host_is_sandbox_denied() {
    use_real_forge();
    let base = tempfile::tempdir().unwrap();
    bare_repo(base.path());
    let Some((_daemon, port)) = git_daemon(base.path()) else {
        return;
    };
    let ws = workspace();
    let (proxy, ctx) = ctx(ws.path(), "github.com", true).await;
    let _ = proxy.shared().drain_events();

    let error = fetch(&ctx, &format!("git://127.0.0.1:{port}/repo.git"))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ToolError::SandboxDenied { denied_host: Some(h), .. } if h == "127.0.0.1"),
        "{error:?}"
    );
    assert!(proxy.shared().drain_events().is_empty());
}

/// The pre-check passes (a `*` grant permits the name) but the gateway
/// refuses the loopback address. Git therefore fails: had git:// gone
/// direct, the daemon would have served it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_protocol_cannot_bypass_the_gateway_address_check() {
    use_real_forge();
    let base = tempfile::tempdir().unwrap();
    bare_repo(base.path());
    let Some((_daemon, port)) = git_daemon(base.path()) else {
        return;
    };
    let ws = workspace();
    let (proxy, ctx) = ctx(ws.path(), "*", false).await;
    let _ = proxy.shared().drain_events();

    let out = fetch(&ctx, &format!("git://127.0.0.1:{port}/repo.git"))
        .await
        .unwrap();
    assert!(out.is_error, "{}", out.content);
    assert!(
        out.content.contains("private address"),
        "the gateway's reason reaches git's output: {}",
        out.content
    );
    let all = proxy.shared().drain_events();
    // A CI runner can carry its own git setup (e.g. a credential helper or
    // cache proxy) whose connections also reach this gateway. Every one of
    // them must be refused; exactly one is git's own connection to the server.
    assert!(all.iter().all(|e| !e.allowed()), "{all:?}");
    let events: Vec<_> = all.into_iter().filter(|e| e.host == "127.0.0.1").collect();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(
        events[0].outcome,
        forge_tools::egress::EgressOutcome::Denied(
            forge_tools::egress::EgressDenial::PrivateAddress
        )
    );
    assert_eq!(events[0].label.as_deref(), Some(CALL_ID));
}

/// An "ssh" with no crypto: it honours `-o ProxyCommand` and `-p` the way
/// OpenSSH does (`%h`, `%p`, `%%`), maps the alias `forge-alias` to
/// `127.0.0.1` the way a `HostName` line would, logs the remote command, and
/// becomes the ProxyCommand, so git's stdio is the tunnel. Without a
/// ProxyCommand it cannot connect at all.
const FAKE_SSH: &str = r#"#!/bin/sh
proxy= port=22
while [ $# -gt 0 ]; do
  case "$1" in
    -o) case "$2" in ProxyCommand=*) proxy=${2#ProxyCommand=} ;; esac; shift 2 ;;
    -p) port=$2; shift 2 ;;
    -*) shift ;;
    *) break ;;
  esac
done
host=${1#*@}
shift
[ "$host" = forge-alias ] && host=127.0.0.1
printf '%s\n' "$*" >> "$FAKE_SSH_LOG"
[ -n "$proxy" ] || { echo "fake ssh: no ProxyCommand" >&2; exit 255; }
command= rest=$proxy
while :; do
  case "$rest" in
    *%*)
      command=$command${rest%%"%"*}
      rest=${rest#*"%"}
      token=${rest%"${rest#?}"}
      rest=${rest#?}
      case "$token" in
        h) command=$command$host ;;
        p) command=$command$port ;;
        %) command=$command% ;;
        *) echo "fake ssh: unknown token %$token" >&2; exit 255 ;;
      esac ;;
    *) command=$command$rest; break ;;
  esac
done
exec sh -c "$command"
"#;

/// Point the workspace's ssh at [`FAKE_SSH`]; returns the remote-command log.
fn use_fake_ssh(ws: &Path, scripts: &Path) -> PathBuf {
    let ssh = scripts.join("fake-ssh");
    let log = scripts.join("remote-commands.log");
    std::fs::write(
        &ssh,
        FAKE_SSH.replacen(
            "#!/bin/sh\n",
            &format!("#!/bin/sh\nFAKE_SSH_LOG='{}'\n", log.display()),
            1,
        ),
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
    git(
        ws,
        &["config", "core.sshCommand", &format!("'{}'", ssh.display())],
    );
    git(ws, &["config", "ssh.variant", "ssh"]);
    log
}

/// The far side of the fake ssh: every connection gets `git upload-pack`
/// for `repo`, with its stdin and stdout bound to the socket.
fn upload_pack_server(repo: PathBuf) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let (Ok(input), Ok(output)) = (stream.try_clone(), stream.try_clone()) else {
                continue;
            };
            let child = Command::new("git")
                .arg("upload-pack")
                .arg(&repo)
                .stdin(Stdio::from(OwnedFd::from(input)))
                .stdout(Stdio::from(OwnedFd::from(output)))
                .stderr(Stdio::null())
                .spawn();
            // Only the child holds the socket now, so its exit closes it.
            drop(stream);
            if let Ok(mut child) = child {
                std::thread::spawn(move || child.wait());
            }
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ssh_fetch_runs_through_the_proxy_command_and_gateway() {
    use_real_forge();
    let base = tempfile::tempdir().unwrap();
    let head = bare_repo(base.path());
    let port = upload_pack_server(base.path().join("repo.git"));
    let ws = workspace();
    let log = use_fake_ssh(ws.path(), base.path());
    let (proxy, ctx) = ctx(ws.path(), "127.0.0.1", true).await;

    let out = fetch(&ctx, &format!("ssh://git@127.0.0.1:{port}/repo.git"))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(git(ws.path(), &["rev-parse", "FETCH_HEAD"]), head);
    assert_one_tunnel(&proxy, port);
    let commands = std::fs::read_to_string(log).unwrap();
    assert!(
        commands.contains("git-upload-pack '/repo.git'"),
        "{commands}"
    );
}

/// The pre-check sees the alias, which the policy permits; ssh resolves it to
/// a host the policy does not, and only the gateway can see that. Its refusal
/// still becomes the structured host denial, naming the real host.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ssh_alias_to_an_unpermitted_host_is_refused_by_the_gateway() {
    use_real_forge();
    let base = tempfile::tempdir().unwrap();
    bare_repo(base.path());
    let port = upload_pack_server(base.path().join("repo.git"));
    let ws = workspace();
    let log = use_fake_ssh(ws.path(), base.path());
    let (proxy, ctx) = ctx(ws.path(), "forge-alias", true).await;

    let error = fetch(&ctx, &format!("ssh://git@forge-alias:{port}/repo.git"))
        .await
        .unwrap_err();
    match &error {
        ToolError::SandboxDenied {
            denied_host,
            content,
            ..
        } => {
            assert_eq!(denied_host.as_deref(), Some("127.0.0.1"));
            assert!(
                content.contains("host 127.0.0.1 is not allowed"),
                "the helper's stderr reaches the report: {content}"
            );
        }
        other => panic!("expected a gateway host denial, got {other:?}"),
    }
    assert!(
        std::fs::read_to_string(log).is_ok(),
        "git must have reached ssh"
    );
    let all = proxy.shared().drain_events();
    // A CI runner can carry its own git setup (e.g. a credential helper or
    // cache proxy) whose connections also reach this gateway. Every one of
    // them must be refused; exactly one is git's own connection to the server.
    assert!(all.iter().all(|e| !e.allowed()), "{all:?}");
    let events: Vec<_> = all.into_iter().filter(|e| e.host == "127.0.0.1").collect();
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(!events[0].allowed());
    assert_eq!(events[0].host, "127.0.0.1");
    assert_eq!(events[0].label.as_deref(), Some(CALL_ID));
}

/// A one-request-per-connection HTTP server that runs `git http-backend`
/// as CGI for `project_root`. Returns `None` when this git has no
/// http-backend.
fn http_backend_server(project_root: PathBuf) -> Option<u16> {
    let exec_path = Command::new("git").arg("--exec-path").output().ok()?;
    let exec_path = PathBuf::from(String::from_utf8_lossy(&exec_path.stdout).trim());
    if !exec_path.join("git-http-backend").exists() {
        eprintln!("skipping: this git has no http-backend");
        return None;
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let root = project_root.clone();
            std::thread::spawn(move || {
                let _ = serve_cgi(stream, &root);
            });
        }
    });
    Some(port)
}

fn serve_cgi(stream: TcpStream, root: &Path) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default();
    // Origin-form is what the gateway forwards; tolerate absolute-form too.
    let target = match target.split_once("://") {
        Some((_, rest)) => &rest[rest.find('/').unwrap_or(rest.len())..],
        None => target,
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };
    let mut body = Vec::new();
    if let Some(length) = header("content-length") {
        body.resize(length.parse().unwrap_or(0), 0);
        reader.read_exact(&mut body)?;
    } else if header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size)?;
            let size = usize::from_str_radix(size.trim().split(';').next().unwrap_or("0"), 16)
                .unwrap_or(0);
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk)?;
            if size == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..size]);
        }
    }

    let mut cgi = Command::new("git");
    cgi.arg("http-backend")
        .env("GIT_PROJECT_ROOT", root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REQUEST_METHOD", &method)
        .env("PATH_INFO", path)
        .env("QUERY_STRING", query)
        .env("REMOTE_ADDR", "127.0.0.1")
        .env("CONTENT_LENGTH", body.len().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(content_type) = header("content-type") {
        cgi.env("CONTENT_TYPE", content_type);
    }
    if let Some(encoding) = header("content-encoding") {
        cgi.env("HTTP_CONTENT_ENCODING", encoding);
    }
    let mut child = cgi.spawn()?;
    child.stdin.take().unwrap().write_all(&body)?;
    let output = child.wait_with_output()?;

    // CGI headers end at the first blank line; `Status:` becomes the status.
    let split = output
        .stdout
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, i + 4))
        .or_else(|| {
            output
                .stdout
                .windows(2)
                .position(|w| w == b"\n\n")
                .map(|i| (i, i + 2))
        })
        .unwrap_or((output.stdout.len(), output.stdout.len()));
    let head = String::from_utf8_lossy(&output.stdout[..split.0]);
    let mut status = "200 OK".to_string();
    let mut response = String::new();
    for line in head.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match line.split_once(':') {
            Some((name, value)) if name.eq_ignore_ascii_case("status") => {
                status = value.trim().to_string();
            }
            _ => {
                response.push_str(line);
                response.push_str("\r\n");
            }
        }
    }
    let mut stream = stream;
    stream.write_all(
        format!("HTTP/1.1 {status}\r\n{response}Connection: close\r\n\r\n").as_bytes(),
    )?;
    stream.write_all(&output.stdout[split.1..])?;
    stream.shutdown(std::net::Shutdown::Both)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_fetch_runs_through_the_gateway() {
    let base = tempfile::tempdir().unwrap();
    let head = bare_repo(base.path());
    let Some(port) = http_backend_server(base.path().to_path_buf()) else {
        return;
    };
    let ws = workspace();
    let (proxy, ctx) = ctx(ws.path(), "127.0.0.1", true).await;

    let out = fetch(&ctx, &format!("http://127.0.0.1:{port}/repo.git"))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(git(ws.path(), &["rev-parse", "FETCH_HEAD"]), head);
    // Smart HTTP is two requests (refs, then the pack), each its own
    // forwarded exchange, both attributed to this call.
    let events = proxy.shared().drain_events();
    let forwarded: Vec<_> = events.iter().filter(|e| e.port == port).collect();
    assert!(forwarded.len() >= 2, "{events:?}");
    for event in forwarded {
        assert!(event.allowed(), "{event:?}");
        assert_eq!(event.host, "127.0.0.1");
        assert_eq!(event.label.as_deref(), Some(CALL_ID));
    }
}
