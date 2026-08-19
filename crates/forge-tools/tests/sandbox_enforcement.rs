//! End-to-end confinement tests.
//!
//! The unit tests in `sandbox.rs` assert the *profile text* is shaped
//! correctly. These assert the **kernel actually refuses**, by spawning a real
//! process through the real mechanism. A profile that reads correctly but does
//! not confine is the failure mode worth spending a slow test on.
//!
//! These replace the negative coverage that will be deleted along with
//! `readonly.rs` — `ls | tee out`, `cat f | sh` and friends were guarded there
//! by parsing. Here they are guarded by the OS, which is the point of the
//! change: the guarantee no longer depends on recognising a command.

use std::path::Path;
use std::process::Command;

use forge_tools::sandbox::{availability, wrap_shell_command, SandboxPolicy};

/// Skip, with the reason on stderr, when this host cannot confine. A silent
/// skip would let the whole suite quietly stop testing anything — the failure
/// mode where coverage evaporates and the badge stays green.
macro_rules! require_sandbox {
    () => {
        if let Err(unavailable) = availability() {
            eprintln!(
                "SKIP {}: {}",
                std::panic::Location::caller().file(),
                unavailable.reason()
            );
            return;
        }
    };
}

/// Run `command` confined to `root`, returning (exit_ok, stdout+stderr).
fn run_confined(root: &Path, command: &str) -> (bool, String) {
    let policy = SandboxPolicy::for_workspace(root);
    let (program, args) =
        wrap_shell_command("sh", command, &policy).expect("sandbox should be available");
    let out = Command::new(program)
        .args(args)
        .current_dir(root)
        .output()
        .expect("spawn the sandbox wrapper");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
    std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(dir.path().join(".forge/permissions.toml"), "allow = []\n").unwrap();
    dir
}

/// Reports what this host can do, and never fails. Reading it in a CI log is
/// how we learn whether the runner actually has bubblewrap — a question argv
/// assertions cannot answer.
#[test]
fn report_sandbox_availability_on_this_host() {
    match availability() {
        Ok(()) => eprintln!("SANDBOX: available on {}", std::env::consts::OS),
        Err(unavailable) => eprintln!(
            "SANDBOX: unavailable on {} — {}",
            std::env::consts::OS,
            unavailable.reason()
        ),
    }
}

/// macOS ships `sandbox-exec`, so there is no excuse for it being missing.
#[cfg(target_os = "macos")]
#[test]
fn sandbox_is_always_available_on_macos() {
    assert!(availability().is_ok(), "macOS hosts must have sandbox-exec");
}

#[test]
fn writes_inside_the_workspace_succeed() {
    require_sandbox!();
    let ws = workspace();
    let (ok, out) = run_confined(ws.path(), "echo hello > allowed.txt");
    assert!(ok, "in-workspace write must succeed: {out}");
    assert_eq!(
        std::fs::read_to_string(ws.path().join("allowed.txt")).unwrap(),
        "hello\n"
    );
}

/// The trade this model makes: in-workspace destruction is permitted, because
/// it is recoverable and visible. Asserted so the trade stays deliberate.
#[test]
fn in_workspace_destruction_is_permitted_by_design() {
    require_sandbox!();
    let ws = workspace();
    std::fs::create_dir_all(ws.path().join("src")).unwrap();
    std::fs::write(ws.path().join("src/main.rs"), "fn main() {}").unwrap();
    let (ok, out) = run_confined(ws.path(), "rm -rf src");
    assert!(ok, "workspace-write permits this by design: {out}");
    assert!(!ws.path().join("src").exists());
}

#[test]
fn git_directory_is_read_only() {
    require_sandbox!();
    let ws = workspace();
    let (ok, _) = run_confined(ws.path(), "echo clobbered > .git/HEAD");
    assert!(!ok, "the recovery mechanism must not be writable");
    assert_eq!(
        std::fs::read_to_string(ws.path().join(".git/HEAD")).unwrap(),
        "ref: refs/heads/main\n"
    );
}

/// `.forge/permissions.toml` decides what forge allows next session. A
/// confined process that could rewrite it would widen its own permissions
/// using nothing but an ordinary in-workspace write.
#[test]
fn forge_directory_is_read_only_so_permissions_cannot_be_widened() {
    require_sandbox!();
    let ws = workspace();
    let (ok, _) = run_confined(
        ws.path(),
        "echo 'allow = [\"*\"]' > .forge/permissions.toml",
    );
    assert!(!ok, "permissions.toml must not be writable from inside");
    assert_eq!(
        std::fs::read_to_string(ws.path().join(".forge/permissions.toml")).unwrap(),
        "allow = []\n"
    );
}

#[test]
fn writes_outside_the_workspace_are_denied() {
    require_sandbox!();
    let ws = workspace();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("escaped.txt");
    let (ok, _) = run_confined(
        ws.path(),
        &format!("echo escaped > {}", target.to_str().unwrap()),
    );
    assert!(!ok, "writing outside the workspace must be denied");
    assert!(!target.exists());
}

/// `cd ..` does not escape: the boundary is the path, not the working
/// directory. This is the distinction `current_dir()` alone never provided.
#[test]
fn leaving_the_workspace_with_cd_does_not_escape() {
    require_sandbox!();
    let ws = workspace();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("via-cd.txt");
    let (ok, _) = run_confined(
        ws.path(),
        &format!(
            "cd {} && echo x > via-cd.txt",
            outside.path().to_str().unwrap()
        ),
    );
    assert!(!ok, "cd must not widen the write boundary");
    assert!(!target.exists());
}

#[test]
fn network_egress_is_denied() {
    require_sandbox!();
    let ws = workspace();
    let (ok, _) = run_confined(ws.path(), "curl -s -m 5 https://example.com");
    assert!(!ok, "network egress must be denied");
}

/// Reads outside the workspace are permitted — toolchains need `~/.gitconfig`
/// and `~/.cargo`. Pinned so the choice is visible rather than assumed: a
/// secret can be read, but the network denial is what stops it leaving.
#[test]
fn reads_outside_the_workspace_are_permitted_by_design() {
    require_sandbox!();
    let ws = workspace();
    let (ok, out) = run_confined(ws.path(), "head -c 5 /etc/hosts > read.txt && echo READ");
    assert!(ok, "broad reads are intentional: {out}");
    assert!(ws.path().join("read.txt").exists());
}

/// The shapes `readonly.rs` used to gate by parsing. None of them are
/// recognised here — they simply cannot reach anything.
#[test]
fn commands_that_used_to_need_parsing_are_contained_instead() {
    require_sandbox!();
    let ws = workspace();
    let outside = tempfile::tempdir().unwrap();
    let escape = outside.path().join("out.txt");
    let escape = escape.to_str().unwrap();

    for command in [
        format!("ls | tee {escape}"),
        format!("cat /etc/hosts | sh -c 'cat > {escape}'"),
        format!("echo x > {escape}"),
        format!("printf 'x' >> {escape}"),
    ] {
        let (ok, _) = run_confined(ws.path(), &command);
        assert!(!ok, "must be contained: {command}");
        assert!(
            !Path::new(escape).exists(),
            "must not have escaped via: {command}"
        );
    }
}

// ---------------------------------------------------------------------------
// The tools themselves, not just the wrapper. These are the assertions that
// would regress if someone added a spawn path and forgot to confine it.
// ---------------------------------------------------------------------------

use forge_tools::run_shell_command;

fn escape_target(outside: &tempfile::TempDir) -> String {
    outside.path().join("escaped.txt").to_str().unwrap().into()
}

#[tokio::test]
async fn bash_tool_is_confined() {
    require_sandbox!();
    let ws = workspace();
    let outside = tempfile::tempdir().unwrap();
    let target = escape_target(&outside);

    let error = run_shell_command(&format!("echo pwned > {target}"), ws.path())
        .await
        .expect_err("escaping the workspace must be denied");
    assert!(
        matches!(error, forge_tools::ToolError::SandboxDenied { .. }),
        "escaping the workspace must report a sandbox denial: {error}"
    );
    assert!(
        !std::path::Path::new(&target).exists(),
        "bash tool escaped the sandbox"
    );
}

#[tokio::test]
async fn bash_tool_still_works_inside_the_workspace() {
    require_sandbox!();
    let ws = workspace();
    let out = run_shell_command("echo hello > inside.txt && cat inside.txt", ws.path())
        .await
        .unwrap();
    assert!(!out.is_error, "in-workspace work must be unaffected");
    assert!(out.content.contains("hello"));
}

#[tokio::test]
async fn bash_tool_cannot_write_git() {
    require_sandbox!();
    let ws = workspace();
    let error = run_shell_command("echo clobbered > .git/HEAD", ws.path())
        .await
        .expect_err("the recovery mechanism must stay read-only");
    assert!(
        matches!(error, forge_tools::ToolError::SandboxDenied { .. }),
        "writing .git must report a sandbox denial: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(ws.path().join(".git/HEAD")).unwrap(),
        "ref: refs/heads/main\n"
    );
}

/// A denied command must say *which boundary* stopped it. Without this the
/// model sees a DNS error or a file-permission error and chases the wrong fix.
#[tokio::test]
async fn a_denied_command_explains_which_boundary_stopped_it() {
    require_sandbox!();
    let ws = workspace();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("nope.txt");

    let error = run_shell_command(&format!("echo x > {}", target.to_str().unwrap()), ws.path())
        .await
        .unwrap_err();

    let forge_tools::ToolError::SandboxDenied { content, reason } = error else {
        panic!("expected a structured sandbox denial");
    };
    assert!(content.contains("blocked by the sandbox"), "{content}");
    assert!(reason.contains("writes are confined"), "{reason}");
}

/// And an ordinary failure must not be dressed up as a sandbox problem.
#[tokio::test]
async fn an_ordinary_failure_is_not_blamed_on_the_sandbox() {
    require_sandbox!();
    let ws = workspace();
    let out = run_shell_command("exit 3", ws.path()).await.unwrap();
    assert!(out.is_error);
    assert!(
        !out.content.contains("blocked by the sandbox"),
        "must not claim a denial, got: {}",
        out.content
    );
}

// ---------------------------------------------------------------------------
// Egress routing. Granting network access means opening exactly one hole — to
// the proxy — and no other. Setting HTTPS_PROXY is not a boundary; a process
// free to open sockets ignores it. These assert the hole is the only route.
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod egress_routing {
    use super::*;
    use forge_tools::sandbox::seatbelt_profile;

    #[test]
    fn without_a_proxy_the_network_is_denied_outright() {
        let ws = workspace();
        let profile = seatbelt_profile(&SandboxPolicy::for_workspace(ws.path())).unwrap();
        assert!(profile.contains("(deny network*)"));
        assert!(
            !profile.contains("network-outbound"),
            "no proxy means no hole at all"
        );
    }

    /// The deny must come first or the allow never takes effect — the same
    /// last-match-wins hazard as the .git carve-out.
    #[test]
    fn the_proxy_hole_is_opened_after_the_blanket_denial() {
        let ws = workspace();
        let profile =
            seatbelt_profile(&SandboxPolicy::for_workspace(ws.path()).with_egress_proxy(9418))
                .unwrap();
        let deny = profile
            .find("(deny network*)")
            .expect("still denies by default");
        let allow = profile
            .find("(allow network-outbound (remote ip \"localhost:9418\"))")
            .expect("the proxy port must be reachable");
        assert!(deny < allow, "an allow before the deny would be overridden");
    }

    /// A real confined process: the proxy port is reachable, and a different
    /// port on the same host is not. This is the assertion that separates
    /// "routed through the proxy" from "network is on".
    #[tokio::test]
    async fn only_the_proxy_port_is_reachable() {
        require_sandbox!();
        let ws = workspace();

        // Two listeners on loopback. Only one is named in the policy.
        let permitted = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let forbidden = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let permitted_port = permitted.local_addr().unwrap().port();
        let forbidden_port = forbidden.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let _ = permitted.accept().await;
            }
        });
        tokio::spawn(async move {
            loop {
                let _ = forbidden.accept().await;
            }
        });

        let policy = SandboxPolicy::for_workspace(ws.path()).with_egress_proxy(permitted_port);
        let probe = |port: u16| {
            let (program, args) =
                wrap_shell_command("sh", &format!("exec 3<>/dev/tcp/127.0.0.1/{port}"), &policy)
                    .unwrap();
            Command::new(program)
                .args(args)
                .current_dir(ws.path())
                .output()
                .unwrap()
                .status
                .success()
        };

        assert!(probe(permitted_port), "the proxy port must be reachable");
        assert!(
            !probe(forbidden_port),
            "every other destination must stay denied, or the allowlist is advisory"
        );
    }
}

/// The outcome the whole egress design exists for, asserted end to end on a
/// real confined command:
///
///   * an allowlisted host is reachable, with no prompt
///   * every other host is not
///   * without a grant, the network is off entirely
///
/// Uses a local listener as the "allowed host", so it does not depend on the
/// internet being up.
/// Egress, end to end, on whichever platform this is.
///
/// Deliberately not macOS-gated any more. The two platforms route to the proxy
/// completely differently — macOS opens one hole in the Seatbelt profile to a
/// loopback port, Linux bind-mounts a Unix socket past `--unshare-net` and runs
/// `socat` inside the namespace to present it as TCP — and the Linux half had
/// never been executed anywhere. It existed as argv assertions written on a
/// Mac, which is the exact shape that produced four Linux defects in a day.
///
/// The command finds the proxy through `$HTTP_PROXY`, which is the address the
/// sandbox actually gave it, so the same test body works on both.
///
/// Uses a local listener as the "allowed host", so it never needs the internet.
#[tokio::test]
async fn a_granted_command_reaches_only_allowlisted_hosts() {
    require_sandbox!();
    use forge_tools::egress::{EgressPolicy, EgressProxy};
    use forge_tools::run_shell_command_with_egress;
    use forge_tools::sandbox::EgressGrant;

    let ws = workspace();
    let sockdir = tempfile::tempdir().unwrap();

    // Stand-in for "the allowed host".
    let allowed = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let allowed_port = allowed.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = allowed.accept().await {
            use tokio::io::AsyncWriteExt;
            let _ = sock.write_all(b"REACHED").await;
        }
    });

    let mut policy = EgressPolicy::new();
    policy.allow("127.0.0.1");
    let mut proxy = EgressProxy::start(policy.clone()).await.unwrap();
    let socket_path = sockdir.path().join("egress.sock");
    proxy
        .serve_on_unix_socket(&socket_path, policy)
        .await
        .unwrap();
    let grant = EgressGrant {
        proxy_port: proxy.addr().port(),
        socket_path,
    };

    // Reaching the allowed host *through the proxy* succeeds. The address comes
    // from the environment the sandbox set, so this does not assume which
    // routing the platform used.
    // bash's /dev/tcp wants HOST/PORT, so split the proxy address rather than
    // pasting it in whole.
    let reach_allowed = [
        r#"addr="${HTTP_PROXY#http://}"; addr="${addr%/}";"#,
        r#"h="${addr%%:*}"; p="${addr##*:}";"#,
        &format!(
            r#"printf 'CONNECT 127.0.0.1:{allowed_port} HTTP/1.1\r\n\r\n' > /dev/tcp/"$h"/"$p""#
        ),
    ]
    .join(" ");
    let out = run_shell_command_with_egress(&reach_allowed, ws.path(), Some(&grant))
        .await
        .unwrap();
    assert!(
        !out.is_error,
        "the proxy must be reachable from inside the sandbox: {}",
        out.content
    );

    // Any other destination stays denied, even with a grant. This is the line
    // between "routed through the proxy" and "the network is simply on".
    let reach_direct = format!("exec 3<>/dev/tcp/127.0.0.1/{allowed_port}");
    match run_shell_command_with_egress(&reach_direct, ws.path(), Some(&grant)).await {
        Err(forge_tools::ToolError::SandboxDenied { .. }) => {}
        Ok(out) => assert!(
            out.is_error,
            "a direct connection must be denied, or the allowlist is advisory: {}",
            out.content
        ),
        Err(error) => panic!("direct connection failed for the wrong reason: {error}"),
    }
}

/// CI must actually exercise the sandbox, not skip it.
///
/// Every enforcement test above returns early when the host cannot confine, so
/// a runner without bubblewrap would leave them all "passing" while asserting
/// nothing. This turns that silence into a failure: if the dependency step is
/// removed from the workflow, the build breaks here instead of quietly
/// dropping the Linux coverage.
///
/// Scoped to CI so a developer without bubblewrap installed still gets a
/// working local test run.
#[cfg(target_os = "linux")]
#[test]
fn sandbox_is_available_on_linux_in_ci() {
    if std::env::var_os("CI").is_none() {
        return;
    }
    match availability() {
        Ok(()) => {}
        Err(unavailable) => panic!(
            "CI must install the sandbox dependencies, or the enforcement \
             suite silently tests nothing — {}",
            unavailable.reason()
        ),
    }
}

// ---------------------------------------------------------------------------
// The second spawn path.
//
// `exec_command` keeps a shell alive across turns and `write_stdin` feeds it,
// and `write_stdin` is in neither `is_shell_tool()` nor
// `default_hitl_tools()` — so once a session exists the agent writes
// arbitrary commands into it with no further approval. Confinement is the only
// thing standing there, it is applied at spawn and cannot be retrofitted, and
// until now nothing checked it took.
// ---------------------------------------------------------------------------

/// Drive `exec_command` to completion and return its result.
async fn exec_command(
    ws: &Path,
    cmd: &str,
) -> Result<forge_types::ToolOutput, forge_tools::ToolError> {
    use forge_tools::{default_builtins, ToolContext};

    let ctx = ToolContext::new(ws.to_path_buf());
    let tool = default_builtins()
        .into_iter()
        .find(|t| t.name() == "exec_command")
        .expect("exec_command must be a builtin");

    tool.call(
        &ctx,
        serde_json::json!({ "cmd": cmd, "yield_time_ms": 400 }),
    )
    .await
}

#[tokio::test]
async fn exec_command_cannot_write_outside_the_workspace() {
    require_sandbox!();
    let ws = workspace();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("escape-exec.txt");

    let error = exec_command(
        ws.path(),
        &format!("echo pwned > {}", target.to_str().unwrap()),
    )
    .await
    .expect_err("the sandbox denial must reach the tool layer");

    assert!(
        !target.exists(),
        "the persistent-session path escaped the sandbox: {error}"
    );
    assert!(
        matches!(error, forge_tools::ToolError::SandboxDenied { .. }),
        "exec_command must escalate sandbox denials: {error}"
    );
}

#[tokio::test]
async fn exec_command_cannot_write_git() {
    require_sandbox!();
    let ws = workspace();
    let error = exec_command(ws.path(), "echo clobbered > .git/HEAD")
        .await
        .expect_err("the sandbox denial must reach the tool layer");
    assert_eq!(
        std::fs::read_to_string(ws.path().join(".git/HEAD")).unwrap(),
        "ref: refs/heads/main\n",
        "exec_command wrote .git: {error}"
    );
    assert!(
        matches!(error, forge_tools::ToolError::SandboxDenied { .. }),
        "exec_command must escalate sandbox denials: {error}"
    );
}

#[tokio::test]
async fn exec_command_still_works_inside_the_workspace() {
    require_sandbox!();
    let ws = workspace();
    let out = exec_command(
        ws.path(),
        "echo hello > inside-exec.txt && cat inside-exec.txt",
    )
    .await
    .expect("ordinary workspace command should succeed");
    assert!(
        ws.path().join("inside-exec.txt").exists(),
        "ordinary work through this path must be unaffected: {}",
        out.content
    );
}

#[tokio::test]
async fn polled_exec_command_escalates_sandbox_denial() {
    require_sandbox!();
    use forge_tools::{default_builtins, ToolContext};

    let ws = workspace();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("escape-polled-exec.txt");
    let ctx = ToolContext::new(ws.path().to_path_buf());
    let tools = default_builtins();
    let exec = tools
        .iter()
        .find(|tool| tool.name() == "exec_command")
        .expect("exec_command must be a builtin");
    let started = exec
        .call(
            &ctx,
            serde_json::json!({
                "cmd": format!("sleep 0.1; echo pwned > {}", target.display()),
                "yield_time_ms": 10
            }),
        )
        .await
        .expect("the command should still be running after the initial yield");
    let body: serde_json::Value = serde_json::from_str(&started.content).unwrap();
    let session_id = body["session_id"].as_u64().unwrap();
    let write_stdin = tools
        .iter()
        .find(|tool| tool.name() == "write_stdin")
        .expect("write_stdin must be a builtin");
    let error = write_stdin
        .call(
            &ctx,
            serde_json::json!({ "session_id": session_id, "yield_time_ms": 400 }),
        )
        .await
        .expect_err("polling must surface the completed sandbox denial");

    assert!(!target.exists(), "the polled command escaped the sandbox");
    assert!(
        matches!(error, forge_tools::ToolError::SandboxDenied { .. }),
        "write_stdin must escalate the session denial: {error}"
    );
}

/// A granted command must *return*, not merely succeed.
///
/// On Linux the relay is a backgrounded `socat` that lives as long as the
/// sandbox. It inherits the command's stdout and stderr, so a caller reading
/// to EOF — which `Command::output()` does, and which every tool path uses —
/// waits on the relay as well as the command. The relay never exits, so the
/// read never completes: every sandboxed command with egress hung forever
/// rather than returning.
///
/// It presented as CI jobs running for 45 minutes instead of two, with no
/// failing assertion anywhere, because a hang is not a failure. Hence the
/// explicit deadline: this asserts termination, which no other test here does.
#[tokio::test]
async fn a_granted_command_returns_rather_than_hanging() {
    require_sandbox!();
    use forge_tools::egress::{EgressPolicy, EgressProxy};
    use forge_tools::run_shell_command_with_egress;
    use forge_tools::sandbox::EgressGrant;

    let ws = workspace();
    let sockdir = tempfile::tempdir().unwrap();
    let mut policy = EgressPolicy::new();
    policy.allow("127.0.0.1");
    let mut proxy = EgressProxy::start(policy.clone()).await.unwrap();
    let socket_path = sockdir.path().join("egress.sock");
    proxy
        .serve_on_unix_socket(&socket_path, policy)
        .await
        .unwrap();
    let grant = EgressGrant {
        proxy_port: proxy.addr().port(),
        socket_path,
    };

    let finished = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        run_shell_command_with_egress("echo done", ws.path(), Some(&grant)),
    )
    .await;

    let out = finished
        .expect("a granted command must terminate; if this times out the relay is holding the caller's pipes open")
        .unwrap();
    assert!(out.content.contains("done"), "got {:?}", out.content);
}

/// A command that attempts an `AF_UNIX` connect and reports *why* it failed.
///
/// The reason is the whole point. `nc -U` exits 1 with no output whether the
/// socket is denied or simply absent, so a test built on it scores a typo in
/// the path as enforcement. Python distinguishes `PermissionError` from
/// `FileNotFoundError`, which is the difference between a boundary holding and
/// a probe that never reached anything.
#[cfg(unix)]
fn unix_connect_probe(path: &Path) -> String {
    format!(
        "python3 -c 'import socket,sys\n\
         s=socket.socket(socket.AF_UNIX)\n\
         try:\n\
        \x20   s.connect(sys.argv[1])\n\
        \x20   print(\"CONNECTED\")\n\
         except Exception as e:\n\
        \x20   print(\"FAILED\", type(e).__name__)' \"{}\"",
        path.display()
    )
}

/// Binds a listener and leaks the accepting thread.
///
/// Never joined on purpose: when the sandbox holds, nothing connects and
/// `accept` blocks forever — joining deadlocks the test rather than failing
/// it, which is how this test hung the first three times it was run.
#[cfg(unix)]
fn listen_and_leak(path: &Path) {
    use std::os::unix::net::UnixListener;
    let listener = UnixListener::bind(path).expect("bind the probe socket");
    std::thread::spawn(move || {
        let _ = listener.accept();
    });
}

/// A socket in a directory the sandbox masks with a tmpfs.
///
/// `/run` is where the sockets that matter most actually live: `docker.sock`
/// (root on the host), systemd, D-Bus, and `$XDG_RUNTIME_DIR` at
/// `/run/user/$UID`, which is where ssh-agent and gpg-agent land on a modern
/// desktop. Probe there when the test can write to it — the WSL2 job runs as
/// root and can.
///
/// `/run` is root-owned, though, and the Ubuntu job runs as `runner`, so fall
/// back to `/tmp`. Both carry the same `--tmpfs` mask from the same two lines
/// of `bubblewrap_invocation`, so `/tmp` proves the mechanism just as well;
/// that both directories are masked at all is held by `socket_directories_are_masked`.
/// The chosen path is named in the failure message so a failure is never
/// ambiguous about what it probed.
#[cfg(target_os = "linux")]
fn masked_probe_dir() -> std::path::PathBuf {
    let name = format!("forge-uds-probe-{}", std::process::id());
    let preferred = Path::new("/run").join(&name);
    if std::fs::create_dir_all(&preferred).is_ok() {
        return preferred;
    }
    let fallback = Path::new("/tmp").join(&name);
    std::fs::create_dir_all(&fallback).expect("a masked probe dir must be creatable");
    fallback
}

/// A socket under `$HOME`, which the sandbox does *not* mask.
///
/// See `a_confined_command_can_still_reach_a_socket_under_home` for why this
/// is a known gap rather than an assertion.
#[cfg(target_os = "linux")]
fn unmasked_probe_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").expect("HOME must be set");
    let dir = Path::new(&home).join(format!(".forge-uds-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Linux: reachability *is* the boundary, so prove it discriminates.
///
/// A confined process can always *create* an `AF_UNIX` socket — forge cannot
/// block that, because the egress relay running inside the sandbox needs one.
/// So the boundary comes from what the mount namespace exposes, and that is
/// only a boundary if it holds.
///
/// This asserts the guarantee forge actually makes: sockets under the masked
/// directories are unreachable. `/run` is the one that matters — docker.sock is
/// root on the host, and `$XDG_RUNTIME_DIR` under `/run/user/$UID` is where
/// ssh-agent and gpg-agent live. The scope of the guarantee, and what sits
/// outside it, is documented on `SandboxPolicy` and in
/// `a_confined_command_can_still_reach_a_socket_under_home` below.
///
/// Both halves matter. The in-workspace connect must succeed, or the probe
/// cannot tell reachable from unreachable and "everything failed" gets scored
/// as security — which is exactly what the first version of this test did.
///
/// The abstract `AF_UNIX` namespace needs no test: it is scoped to the network
/// namespace, and `--unshare-net` gives the sandbox its own.
#[cfg(target_os = "linux")]
#[test]
fn a_confined_command_reaches_workspace_sockets_but_not_masked_host_sockets() {
    require_sandbox!();

    let ws = workspace();

    let inside = ws.path().join("inside.sock");
    listen_and_leak(&inside);
    let (_, inside_out) = run_confined(ws.path(), &unix_connect_probe(&inside));
    assert!(
        inside_out.contains("CONNECTED"),
        "the probe could not reach a socket inside the workspace, so it cannot \
         tell reachable from unreachable and the assertion below proves \
         nothing.\n{inside_out}"
    );

    let probe_dir = masked_probe_dir();
    let outside = probe_dir.join("probe.sock");
    listen_and_leak(&outside);
    let (_, outside_out) = run_confined(ws.path(), &unix_connect_probe(&outside));
    let _ = std::fs::remove_dir_all(&probe_dir);

    assert!(
        !outside_out.contains("CONNECTED"),
        "a confined command reached a host Unix socket at {}, which sits under \
         a directory the sandbox masks with a tmpfs.\n\
         This is the boundary forge actually claims, and it did not hold — \
         docker.sock and $XDG_RUNTIME_DIR live here.\n{outside_out}",
        outside.display()
    );
}

/// Documents a known gap rather than asserting a guarantee.
///
/// `--ro-bind / /` exposes every host path read-only, and a read-only *mount*
/// does not stop `connect()`: that checks the inode, not `MNT_READONLY`. The
/// sandbox masks `/run` and `/tmp`, so the sockets with the worst blast radius
/// are covered — but a socket anywhere else, `$HOME` most realistically, is
/// still reachable from inside the sandbox.
///
/// Closing this needs a mechanism the mount namespace does not have:
///
///   - seccomp-bpf cannot do it. A filter sees `args[6]` as scalar registers
///     and cannot dereference the `sockaddr_un *` to read the path.
///   - seccomp user-notification can read the target's memory, but
///     `seccomp_unotify(2)` states it "must not be used to make security policy
///     decisions about the system call, which would be inherently race-prone".
///   - Landlock ABI 9 added `LANDLOCK_ACCESS_FS_RESOLVE_UNIX`, which restricts
///     `connect(2)` to pathname sockets and is exactly the right primitive. ABI
///     5 shipped in 6.10, 6 in 6.12, 7 in 6.15, so 9 is far newer than the 6.8
///     kernel on the CI runner and than most users' kernels.
///
/// This test is `#[ignore]`d rather than deleted: it is the executable record
/// of the gap. Run it with `--ignored` to check whether a kernel or a change to
/// the mask has closed it, and when Landlock lands, promote it back to an
/// assertion. Tracked in #390.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "known gap: sockets outside /run and /tmp are reachable; see #390"]
fn a_confined_command_can_still_reach_a_socket_under_home() {
    require_sandbox!();

    let ws = workspace();
    let probe_dir = unmasked_probe_dir();
    let outside = probe_dir.join("probe.sock");
    listen_and_leak(&outside);
    let (_, out) = run_confined(ws.path(), &unix_connect_probe(&outside));
    let _ = std::fs::remove_dir_all(&probe_dir);

    assert!(
        out.contains("CONNECTED"),
        "this test documents a gap by reproducing it. It did not reproduce, \
         which means the boundary is now stronger than recorded — verify why, \
         then promote this back to an assertion that the socket is \
         unreachable.\n{out}"
    );
}

/// macOS: `AF_UNIX` is denied outright, which is stronger than Linux manages.
///
/// A confined command cannot connect to a Unix socket anywhere — not even one
/// inside its own workspace, which fails with `PermissionError`. macOS
/// therefore has no reachable-vs-creatable gap: it does not lean on masking
/// and needs no seccomp equivalent. Nothing legitimate wants this, because the
/// macOS egress path is TCP to a localhost port rather than a socket.
///
/// Asserting the *reason* is what keeps this honest: a probe pointed at a path
/// that does not exist reports `FileNotFoundError` and fails this test rather
/// than passing as enforcement.
#[cfg(target_os = "macos")]
#[test]
fn unix_sockets_are_denied_wholesale() {
    require_sandbox!();

    let ws = workspace();
    let inside = ws.path().join("inside.sock");
    listen_and_leak(&inside);

    let (_, out) = run_confined(ws.path(), &unix_connect_probe(&inside));

    assert!(
        out.contains("PermissionError"),
        "expected Seatbelt to refuse the connect with PermissionError.\n\
         CONNECTED means Unix sockets are now allowed and the Linux-only \
         reachability gap has been reintroduced here; FileNotFoundError means \
         the probe never found the socket and tested nothing.\n{out}"
    );
}

/// A workspace that disappears mid-session must not silently drop confinement.
///
/// `wrap_shell_command` returns `None` both when the host cannot confine at all
/// and when this particular policy could not be built — `canonicalize` fails if
/// the root is gone. The caller treats both the same and runs the command
/// unconfined, but only the first is a decision anyone made: the sandbox-less
/// case is a launch-time refusal in the supported CLI, and nothing
/// re-evaluates that when a directory vanishes at 11pm.
#[tokio::test]
async fn a_vanished_workspace_does_not_silently_run_unconfined() {
    require_sandbox!();

    let ws = workspace();
    let root = ws.path().to_path_buf();
    let marker = root.join("canary");
    std::fs::write(&marker, "x").unwrap();

    // Gone, but the path is still what the session is holding.
    std::fs::remove_dir_all(&root).unwrap();

    let result = forge_tools::run_shell_command("echo escaped", &root).await;

    match result {
        Err(e) => assert!(
            e.to_string().contains("refusing to run unconfined"),
            "must refuse because it could not confine, not incidentally \
             because bash could not chdir — the second stops being true the \
             moment the path is reusable: {e}"
        ),
        Ok(out) => panic!(
            "the command ran with no sandbox and no error after the workspace \
             disappeared. Confinement is applied at spawn, so a command that \
             starts unconfined stays unconfined: {:?}",
            out.content
        ),
    }
}

/// A workspace path that is not valid UTF-8 must refuse, not run unconfined.
///
/// Linux paths are bytes, so this directory is legal and a user can create one
/// by accident. `bubblewrap_invocation` needs `&str` for the argv and gives up
/// when the conversion fails — but the path still works for `current_dir`, so
/// before the guard the command spawned perfectly happily with no sandbox
/// around it, in Auto mode, with nothing to prompt because the session decided
/// long ago that this host could confine.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_non_utf8_workspace_refuses_rather_than_dropping_the_sandbox() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    require_sandbox!();

    let parent = tempfile::tempdir().unwrap();
    // 0xFF is not valid UTF-8 in any position.
    let mut name = OsString::from_vec(b"ws-\xff".to_vec());
    name.push("");
    let root = parent.path().join(name);
    std::fs::create_dir_all(&root).unwrap();
    assert!(root.to_str().is_none(), "the path must not be valid UTF-8");

    let result = forge_tools::run_shell_command("echo escaped", &root).await;

    match result {
        Err(e) => assert!(
            e.to_string().contains("refusing to run unconfined"),
            "must refuse for the sandbox reason, not incidentally: {e}"
        ),
        Ok(out) => panic!(
            "ran unconfined on a host that can confine: {:?}",
            out.content
        ),
    }
}

/// A grant whose proxy is gone must fail closed.
///
/// The grant is handed to the sandbox at spawn and outlives nothing — if the
/// proxy dies, or its socket is removed, the command still starts with proxy
/// environment pointing at a relay with nothing behind it. The failure has to
/// be "cannot reach anything", never "reaches the network directly", because
/// the second would turn a crashed helper into an open egress path.
#[tokio::test]
async fn egress_fails_closed_when_the_proxy_is_gone() {
    use forge_tools::egress::{EgressPolicy, EgressProxy};
    use forge_tools::run_shell_command_with_egress;
    use forge_tools::sandbox::EgressGrant;

    require_sandbox!();

    let ws = workspace();
    let sockdir = tempfile::tempdir().unwrap();
    let socket_path = sockdir.path().join("egress.sock");

    let mut policy = EgressPolicy::new();
    policy.allow("example.com");
    let proxy_port = {
        let mut proxy = EgressProxy::start(policy.clone()).await.unwrap();
        proxy
            .serve_on_unix_socket(&socket_path, policy)
            .await
            .unwrap();
        let port = proxy.addr().port();
        // Drop stops the listeners and removes the socket file: the session's
        // proxy has died while the grant lives on.
        port
    };
    assert!(
        !socket_path.exists(),
        "dropping the proxy must remove its socket"
    );

    let grant = EgressGrant {
        proxy_port,
        socket_path: socket_path.clone(),
    };

    let out = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        run_shell_command_with_egress(
            "curl -s -m 5 -o /dev/null -w '%{http_code}' https://example.com; echo \" rc=$?\"",
            ws.path(),
            Some(&grant),
        ),
    )
    .await
    .expect("a command with a dead proxy must still terminate")
    .unwrap();

    // The command must genuinely have run: a missing curl, or a shell that
    // never started, would also "not contain 200" and would prove nothing.
    assert!(
        out.content.contains("rc="),
        "the probe never executed, so its failure says nothing about egress: {:?}",
        out.content
    );
    assert!(
        !out.content.contains("200"),
        "a dead proxy must not leave a route to the network: {:?}",
        out.content
    );
}
