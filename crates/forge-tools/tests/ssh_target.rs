//! The SSH execution target against a real `sshd` (run as this user, on a
//! loopback port, behind a relay the test can freeze).
//!
//! Every test runs the real tools with an SSH target and an *empty* local
//! workspace: a tool that touched this machine by mistake would find nothing
//! there, and the tests check it stays empty. The server sets
//! `FORGE_TEST_REMOTE=1` in its sessions, so a command that ran locally
//! cannot pass for a remote one. Skipped (loudly) without OpenSSH.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_tools::target::testing::{ServerOptions, SshServer, DOWN_HOST, UNKNOWN_KEY_HOST};
use forge_tools::target::{SshTarget, Target};
use forge_tools::{default_builtins, Tool, ToolContext, ToolError};
use serde_json::{json, Value};

struct Fixture {
    server: SshServer,
    _local: tempfile::TempDir,
    workspace: PathBuf,
    target: Arc<SshTarget>,
    ctx: ToolContext,
}

async fn fixture() -> Option<Fixture> {
    fixture_with(ServerOptions::default()).await
}

async fn fixture_with(options: ServerOptions) -> Option<Fixture> {
    let server = SshServer::start_with(options).await?;
    let local = tempfile::tempdir().unwrap();
    let workspace = local.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let control = local.path().join("ssh");
    std::fs::create_dir_all(&control).unwrap();
    let target = Arc::new(server.target(&control));
    let ctx = ToolContext::new(workspace.clone()).with_target(target.clone());
    Some(Fixture {
        server,
        _local: local,
        workspace,
        target,
        ctx,
    })
}

impl Fixture {
    fn remote(&self, rel: &str) -> PathBuf {
        self.server.remote_root().join(rel)
    }

    async fn call(&self, tool: &str, args: Value) -> Result<forge_types::ToolOutput, ToolError> {
        tool_named(tool).call(&self.ctx, args).await
    }

    fn local_workspace_is_empty(&self) -> bool {
        std::fs::read_dir(&self.workspace).unwrap().next().is_none()
    }
}

fn tool_named(name: &str) -> Arc<dyn Tool> {
    default_builtins()
        .into_iter()
        .find(|tool| tool.name() == name)
        .unwrap_or_else(|| panic!("no tool {name}"))
}

macro_rules! fixture_or_skip {
    () => {
        match fixture().await {
            Some(fixture) => fixture,
            None => return,
        }
    };
}

/// CI installs OpenSSH so these tests run there; without it every test
/// here would skip and the suite would pass having tested nothing.
#[tokio::test]
async fn openssh_is_available_in_ci() {
    if std::env::var_os("CI").is_some() {
        assert!(
            SshServer::start().await.is_some(),
            "CI must install openssh-client and openssh-server"
        );
    }
}

// ---- Task 4: connecting ------------------------------------------------

#[tokio::test]
async fn connecting_finds_the_remote_root_and_a_private_remote_temp() {
    let f = fixture_or_skip!();
    f.target.connect().await.unwrap();
    assert_eq!(
        f.target.remote_root().as_deref(),
        Some(f.server.remote_root().display().to_string().as_str())
    );
    let temp = f.target.remote_temp().expect("a remote temp");
    assert!(Path::new(&temp).is_dir(), "made on the host");
    assert!(!temp.starts_with(&f.server.remote_root().display().to_string()));
    assert_eq!(f.target.remote_host(), Some("forge-test"));
    assert_eq!(
        f.target.label(),
        format!("forge-test:{}", f.server.remote_root().display())
    );
    f.target.close().await;
    assert!(
        !Path::new(&temp).exists(),
        "the remote temp goes with the session"
    );
}

#[tokio::test]
async fn nothing_of_this_machine_is_forwarded_whatever_the_config_says() {
    let f = fixture_or_skip!();
    // The test config sets ForwardAgent yes, ForwardX11 yes and SendEnv *,
    // and the server accepts any environment.
    let config = f.target.effective_config().await.unwrap().to_lowercase();
    for line in [
        "forwardagent no",
        "forwardx11 no",
        "clearallforwardings yes",
        "batchmode yes",
        "serveraliveinterval 1",
        "serveralivecountmax 2",
    ] {
        assert!(
            config.lines().any(|l| l == line),
            "missing `{line}` in:\n{config}"
        );
    }
    let forced = f.target.forced_options().join(" ");
    assert!(forced.contains("ControlMaster"), "{forced}");
    assert!(forced.contains("BatchMode=yes"), "{forced}");
    // `SendEnv` cannot be cleared from the command line, so what `ssh` is
    // given to send is nothing: Forge's own environment never reaches it.
    // SAFETY: a name no other test reads.
    std::env::set_var("FORGE_TEST_MUST_NOT_LEAK", "leaked");
    let out = f
        .call(
            "bash",
            // A forwarded agent is a socket `sshd` makes: `ssh-*/agent.<pid>`.
            // A macOS host sets its own `SSH_AUTH_SOCK` (launchd's) in every
            // session, which is the host's, not this machine's.
            json!({"command": "test \"$FORGE_TEST_REMOTE\" = 1 || exit 99; echo \"[$FORGE_TEST_MUST_NOT_LEAK]\"; case ${SSH_AUTH_SOCK:-} in */ssh-*/agent.*) echo \"forwarded $SSH_AUTH_SOCK\" ;; esac"}),
        )
        .await
        .unwrap();
    assert_eq!(out.exit_code, Some(0), "{}", out.content);
    assert_eq!(
        out.content.trim(),
        "[]",
        "no environment, and no agent, reached the host"
    );
}

async fn connect_error(f: &Fixture, host: &str, path: &str) -> String {
    let control = f._local.path().join(format!("ssh-{host}"));
    std::fs::create_dir_all(&control).unwrap();
    let target = SshTarget::new(host, path, &control, f.server.options());
    match target.connect().await {
        Ok(()) => panic!("{host}:{path} should not connect"),
        Err(error) => error.to_string(),
    }
}

#[tokio::test]
async fn each_connection_problem_is_one_clear_error() {
    let f = fixture_or_skip!();
    let root = f.server.remote_root().display().to_string();
    let unknown_key = connect_error(&f, UNKNOWN_KEY_HOST, &root).await;
    assert!(unknown_key.contains("host key"), "{unknown_key}");
    assert!(unknown_key.contains(UNKNOWN_KEY_HOST), "{unknown_key}");

    let started = Instant::now();
    let down = connect_error(&f, DOWN_HOST, &root).await;
    assert!(
        down.contains(&format!("Could not connect to {DOWN_HOST}")),
        "{down}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "bounded by the connect timeout"
    );

    let missing = connect_error(&f, "forge-test", "/no/such/folder").await;
    assert!(
        missing.contains("/no/such/folder") && missing.contains("does not exist"),
        "{missing}"
    );

    std::fs::write(f.remote("a-file"), "x").unwrap();
    let file = connect_error(&f, "forge-test", &f.remote("a-file").display().to_string()).await;
    assert!(file.contains("is not a folder"), "{file}");

    let relative = connect_error(&f, "forge-test", "relative/path").await;
    assert!(relative.contains("absolute"), "{relative}");
}

#[tokio::test]
async fn a_host_without_the_needed_tools_is_refused_by_name() {
    // A PATH with no realpath: the preflight names what is missing.
    let empty = tempfile::tempdir().unwrap();
    let bin = empty.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for tool in [
        "bash", "uname", "mktemp", "setsid", "cat", "mkdir", "rm", "kill", "sleep",
    ] {
        if let Ok(found) = which(tool) {
            std::os::unix::fs::symlink(found, bin.join(tool)).unwrap();
        }
    }
    let Some(server) = SshServer::start_with(ServerOptions {
        path_prefix: vec![bin.clone()],
        only_path: true,
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let local = tempfile::tempdir().unwrap();
    let target = server.target(local.path());
    let error = target.connect().await.unwrap_err().to_string();
    assert!(error.contains("realpath"), "{error}");
}

fn which(tool: &str) -> Result<PathBuf, ()> {
    for dir in ["/usr/bin", "/bin", "/usr/local/bin"] {
        let candidate = Path::new(dir).join(tool);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(())
}

// ---- Task 5: processes -------------------------------------------------

#[tokio::test]
async fn bash_runs_on_the_host_in_the_remote_root() {
    let f = fixture_or_skip!();
    let out = f
        .call(
            "bash",
            json!({"command": "pwd; echo remote=$FORGE_TEST_REMOTE"}),
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(
        out.content.trim(),
        format!("{}\nremote=1", f.server.remote_root().display())
    );
    assert!(f.local_workspace_is_empty());
}

#[tokio::test]
async fn commands_get_the_remote_temp_as_tmpdir() {
    let f = fixture_or_skip!();
    let out = f
        .call(
            "bash",
            json!({"command": "test \"$FORGE_TEST_REMOTE\" = 1 || exit 99; echo $TMPDIR"}),
        )
        .await
        .unwrap();
    assert_eq!(out.exit_code, Some(0), "{}", out.content);
    let temp = f
        .target
        .remote_temp()
        .expect("connected by the first command");
    assert_eq!(out.content.trim(), temp);
}

#[tokio::test]
async fn exit_codes_and_stderr_come_back_as_they_do_locally() {
    let f = fixture_or_skip!();
    for (command, code) in [
        ("true", 0),
        ("false", 1),
        ("echo out; echo err >&2; exit 42", 42),
        ("definitely-not-a-command-xyz", 127),
        ("exit 255", 255),
    ] {
        // Exit 99 unless this runs on the host: a local run cannot pass.
        let remote_only = format!("test \"$FORGE_TEST_REMOTE\" = 1 || exit 99; {command}");
        let out = f
            .call("bash", json!({ "command": remote_only }))
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(code), "{command}: {}", out.content);
        assert_eq!(out.is_error, code != 0, "{command}");
        if code == 42 {
            assert!(
                out.content.contains("out") && out.content.contains("err"),
                "{}",
                out.content
            );
        }
    }
}

#[tokio::test]
async fn output_is_capped_as_it_is_locally() {
    let f = fixture_or_skip!();
    let out = f
        .call(
            "bash",
            json!({"command": "test \"$FORGE_TEST_REMOTE\" = 1 || exit 99; head -c 700000 /dev/zero | tr '\\0' x"}),
        )
        .await
        .unwrap();
    assert_eq!(out.exit_code, Some(0), "ran on the host");
    assert!(
        out.content.len() <= 512 * 1024 + 16,
        "{} bytes",
        out.content.len()
    );
    assert!(out.content.len() >= 500 * 1024);
}

#[tokio::test]
async fn exec_command_pipes_yield_and_take_stdin_on_the_host() {
    let f = fixture_or_skip!();
    // One registry's pair: they share the session store.
    let tools = default_builtins();
    let named = |name: &str| {
        tools
            .iter()
            .find(|tool| tool.name() == name)
            .unwrap()
            .clone()
    };
    let (exec, stdin) = (named("exec_command"), named("write_stdin"));
    let out = exec
        .call(
            &f.ctx,
            json!({"cmd": "echo ready $FORGE_TEST_REMOTE; read line; echo got:$line", "yield_time_ms": 800}),
        )
        .await
        .unwrap();
    let body: Value = serde_json::from_str(&out.content).unwrap();
    assert!(
        body["output"].as_str().unwrap().contains("ready 1"),
        "{body}"
    );
    let session = body["session_id"]
        .as_u64()
        .expect("still running after the yield");
    let out = stdin
        .call(
            &f.ctx,
            json!({"session_id": session, "chars": "hello\n", "yield_time_ms": 2000}),
        )
        .await
        .unwrap();
    let body: Value = serde_json::from_str(&out.content).unwrap();
    assert!(
        body["output"].as_str().unwrap().contains("got:hello"),
        "{body}"
    );
    assert!(
        body["output"]
            .as_str()
            .unwrap()
            .contains("[process exited with code 0]"),
        "{body}"
    );
}

#[tokio::test]
async fn exec_command_with_a_tty_gets_a_terminal_on_the_host() {
    let f = fixture_or_skip!();
    if !f.server.allocates_terminals().await {
        eprintln!("SKIPPED: this sshd cannot allocate a terminal (it is not running as root)");
        return;
    }
    let out = tool_named("exec_command")
        .call(
            &f.ctx,
            json!({"cmd": "tty; echo r=$FORGE_TEST_REMOTE", "tty": true, "yield_time_ms": 3000}),
        )
        .await
        .unwrap();
    let body: Value = serde_json::from_str(&out.content).unwrap();
    let output = body["output"].as_str().unwrap();
    // Linux names terminals `/dev/pts/N`, macOS `/dev/ttysN`.
    assert!(
        output.contains("/dev/pts/") || output.contains("/dev/ttys"),
        "{output}"
    );
    assert!(output.contains("r=1"), "{output}");
}

/// Start a remote command that records its pid, cancel it, and return the
/// pid file.
fn pid_alive(pid_file: &Path) -> bool {
    let pid = std::fs::read_to_string(pid_file).unwrap();
    std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn wait_for(mut check: impl FnMut() -> bool, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn cancelling_bash_kills_the_command_on_the_host() {
    let f = fixture_or_skip!();
    let pid_file = f.remote("sleeper.pid");
    let command = format!("echo $$ > {}; exec sleep 300", pid_file.display());
    let call = f.call("bash", json!({ "command": command }));
    let _ = tokio::time::timeout(Duration::from_millis(1500), call).await;
    assert!(pid_file.exists(), "the command started on the host");
    assert!(
        wait_for(|| !pid_alive(&pid_file), Duration::from_secs(5)).await,
        "dropping the call kills the remote process, not just the local ssh"
    );
}

#[tokio::test]
async fn a_background_job_runs_on_the_host_and_is_killed_with_it() {
    let f = fixture_or_skip!();
    let capture = Arc::new(forge_tools::ShellOutputCapture::default());
    let out =
        forge_tools::run_shell_job(&f.ctx, "echo bg=$FORGE_TEST_REMOTE; pwd", capture.clone())
            .await
            .unwrap();
    assert!(out.content.contains("bg=1"), "{}", out.content);
    assert!(out
        .content
        .contains(&f.server.remote_root().display().to_string()));
    assert!(capture.snapshot().exited);

    let pid_file = f.remote("bg.pid");
    let command = format!("echo $$ > {}; exec sleep 300", pid_file.display());
    let job = forge_tools::run_shell_job(
        &f.ctx,
        &command,
        Arc::new(forge_tools::ShellOutputCapture::default()),
    );
    let _ = tokio::time::timeout(Duration::from_millis(1500), job).await;
    assert!(wait_for(|| !pid_alive(&pid_file), Duration::from_secs(5)).await);
}

#[tokio::test]
async fn a_silent_network_is_an_error_within_the_keepalive_bound_not_a_hang() {
    let f = fixture_or_skip!();
    // Connected and working first.
    let ok = f.call("bash", json!({"command": "echo up"})).await.unwrap();
    assert_eq!(ok.content.trim(), "up");
    let call = f.call("bash", json!({"command": "sleep 30; echo finished"}));
    let freeze_after = async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        f.server.freeze();
    };
    let started = Instant::now();
    let (result, ()) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(20), call),
        freeze_after
    );
    let result = result.expect("the call returned instead of hanging");
    let error = match result {
        Err(error) => error.to_string(),
        Ok(out) => panic!("a lost connection must be an error, got: {}", out.content),
    };
    assert!(
        error.contains("Lost the connection to forge-test while running sleep 30")
            && error.contains("It may still be running there")
            && error.contains("reconnect on the next command"),
        "{error}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(12),
        "{:?}",
        started.elapsed()
    );

    // The network comes back: the next command reconnects.
    f.server.thaw();
    f.server.sever();
    let again = f
        .call("bash", json!({"command": "echo back"}))
        .await
        .unwrap();
    assert_eq!(again.content.trim(), "back");
}

// ---- Task 6: files -----------------------------------------------------

#[tokio::test]
async fn files_are_read_and_written_on_the_host() {
    let f = fixture_or_skip!();
    let out = f
        .call(
            "write_file",
            json!({"path": "src/new/deep.txt", "content": "one\ntwo\nthree\n"}),
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(
        std::fs::read_to_string(f.remote("src/new/deep.txt")).unwrap(),
        "one\ntwo\nthree\n"
    );
    let read = f
        .call("read_file", json!({"path": "src/new/deep.txt"}))
        .await
        .unwrap();
    assert_eq!(read.content, "one\ntwo\nthree");
    let slice = f
        .call(
            "read_file",
            json!({"path": "src/new/deep.txt", "offset": 2, "limit": 1}),
        )
        .await
        .unwrap();
    assert_eq!(slice.content, "two");
    let listing = f.call("ls", json!({"path": "src"})).await.unwrap();
    assert!(listing.content.contains("new"), "{}", listing.content);
    assert!(
        f.local_workspace_is_empty(),
        "nothing was written on this machine"
    );
}

#[tokio::test]
async fn a_large_remote_file_needs_a_range() {
    let f = fixture_or_skip!();
    std::fs::write(f.remote("big.log"), "x\n".repeat(1_200_000)).unwrap();
    let whole = f
        .call("read_file", json!({"path": "big.log"}))
        .await
        .unwrap();
    assert!(
        whole.is_error && whole.content.contains("offset"),
        "{}",
        whole.content
    );
    let ranged = f
        .call(
            "read_file",
            json!({"path": "big.log", "offset": 10, "limit": 3}),
        )
        .await
        .unwrap();
    assert_eq!(ranged.content, "x\nx\nx");
}

#[tokio::test]
async fn edit_and_apply_patch_change_remote_files() {
    let f = fixture_or_skip!();
    std::fs::write(f.remote("main.rs"), "fn main() { println!(\"old\"); }\n").unwrap();
    let edit = f
        .call(
            "edit",
            json!({"path": "main.rs", "old_string": "old", "new_string": "new"}),
        )
        .await
        .unwrap();
    assert!(!edit.is_error, "{}", edit.content);
    assert!(std::fs::read_to_string(f.remote("main.rs"))
        .unwrap()
        .contains("\"new\""));
    let patch = "*** Begin Patch\n*** Add File: added.txt\n+hello\n*** Update File: main.rs\n@@\n-fn main() { println!(\"new\"); }\n+fn main() { println!(\"patched\"); }\n*** End Patch";
    let applied = f
        .call("apply_patch", json!({ "patch": patch }))
        .await
        .unwrap();
    assert!(!applied.is_error, "{}", applied.content);
    assert_eq!(
        std::fs::read_to_string(f.remote("added.txt")).unwrap(),
        "hello\n"
    );
    assert!(std::fs::read_to_string(f.remote("main.rs"))
        .unwrap()
        .contains("patched"));
    assert!(f.local_workspace_is_empty());
}

#[tokio::test]
async fn writes_cannot_leave_the_remote_root() {
    let f = fixture_or_skip!();
    let outside = tempfile::tempdir().unwrap();
    let outside_path = outside.path().canonicalize().unwrap();
    std::os::unix::fs::symlink(&outside_path, f.remote("link-out")).unwrap();
    std::os::unix::fs::symlink(outside_path.join("not-yet"), f.remote("dangling")).unwrap();
    std::fs::create_dir_all(f.remote(".git")).unwrap();
    for path in [
        "../escape.txt".to_string(),
        format!("{}/abs.txt", outside_path.display()),
        "link-out/through-link.txt".to_string(),
        "dangling".to_string(),
        ".git/config".to_string(),
        ".forge/rules.toml".to_string(),
        "a/../../escape.txt".to_string(),
    ] {
        let result = f
            .call("write_file", json!({"path": path, "content": "pwned"}))
            .await;
        assert!(
            result.as_ref().map(|o| o.is_error).unwrap_or(true),
            "writing {path} must be refused: {result:?}"
        );
    }
    assert_eq!(
        std::fs::read_dir(&outside_path).unwrap().count(),
        0,
        "nothing outside was written"
    );
    assert!(!f.remote(".git/config").exists());
    let read_out = f.call("read_file", json!({"path": "link-out/x"})).await;
    assert!(
        read_out.map(|o| o.is_error).unwrap_or(true),
        "reads are confined too"
    );
}

/// `.git` and `.forge` stay unwritable through a symbolic link: file tools
/// never ask, so a hook planted that way would run, unseen, on the next
/// approved `git commit`.
#[tokio::test]
async fn git_internals_cannot_be_written_through_a_link() {
    let f = fixture_or_skip!();
    std::fs::create_dir_all(f.remote(".git/hooks")).unwrap();
    std::fs::write(f.remote(".git/config"), "[core]\n").unwrap();
    std::os::unix::fs::symlink(f.remote(".git/hooks"), f.remote("hooks")).unwrap();
    std::os::unix::fs::symlink(f.remote(".git/config"), f.remote("config-link")).unwrap();
    std::os::unix::fs::symlink(f.remote(".git"), f.remote("dotgit")).unwrap();
    for path in [
        "hooks/pre-commit",
        "config-link",
        "dotgit/hooks/post-checkout",
    ] {
        let result = f
            .call(
                "write_file",
                json!({"path": path, "content": "#!/bin/sh\ntouch pwned\n"}),
            )
            .await;
        assert!(
            result.as_ref().map(|o| o.is_error).unwrap_or(true),
            "writing {path} must be refused: {result:?}"
        );
    }
    assert!(!f.remote(".git/hooks/pre-commit").exists());
    assert!(!f.remote(".git/hooks/post-checkout").exists());
    assert_eq!(
        std::fs::read_to_string(f.remote(".git/config")).unwrap(),
        "[core]\n"
    );
}

/// A name ending in a line break is still that name: resolving it on the
/// host must not drop the break and judge a different, contained path.
#[tokio::test]
async fn a_trailing_line_break_cannot_smuggle_a_path_out() {
    let f = fixture_or_skip!();
    let root = f.server.remote_root();
    // A sibling of the workspace whose name is the workspace's plus "\n".
    let sibling = PathBuf::from(format!("{}\n", root.display()));
    std::fs::create_dir_all(&sibling).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(sibling.clone());
    std::os::unix::fs::symlink(&sibling, f.remote("l")).unwrap();
    let result = f
        .call("write_file", json!({"path": "l/pwned", "content": "x"}))
        .await;
    assert!(
        result.as_ref().map(|o| o.is_error).unwrap_or(true),
        "the write must be refused: {result:?}"
    );
    assert!(
        !sibling.join("pwned").exists(),
        "nothing was written outside"
    );
}

#[tokio::test]
async fn hostile_file_names_are_just_names() {
    let f = fixture_or_skip!();
    let canary = f.remote("pwned");
    for name in [
        "we ird $(touch pwned) 'q\".txt",
        "-rf",
        "`touch pwned`.txt",
        "semi;colon|pipe&.txt",
        "new\nline.txt",
    ] {
        let out = f
            .call("write_file", json!({"path": name, "content": name}))
            .await
            .unwrap();
        assert!(!out.is_error, "{name:?}: {}", out.content);
        assert_eq!(std::fs::read_to_string(f.remote(name)).unwrap(), name);
        let back = f.call("read_file", json!({ "path": name })).await.unwrap();
        assert_eq!(back.content, name.trim_end_matches('\n'));
    }
    assert!(!canary.exists(), "no file name was ever run");
}

/// A 1x1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0, 0, 0,
    1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 0x0d, 0x49, 0x44, 0x41, 0x54,
    0x78, 0x9c, 0x63, 0, 1, 0, 0, 5, 0, 1, 0x0d, 0x0a, 0x2d, 0xb4, 0, 0, 0, 0, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

#[tokio::test]
async fn view_image_reads_the_remote_file() {
    let f = fixture_or_skip!();
    std::fs::write(f.remote("dot.png"), PNG).unwrap();
    let mut ctx = f.ctx.clone();
    ctx.image_input = true;
    let out = tool_named("view_image")
        .call(&ctx, json!({"path": "dot.png"}))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(out.attachments.len(), 1);
}

// ---- Task 7: search and git --------------------------------------------

fn seed_tree(f: &Fixture) {
    std::fs::create_dir_all(f.remote("src/nested")).unwrap();
    std::fs::write(f.remote("src/lib.rs"), "pub fn alpha() {}\n// TODO: beta\n").unwrap();
    std::fs::write(
        f.remote("src/nested/mod.rs"),
        "fn gamma() {}\n// TODO: delta\n",
    )
    .unwrap();
    std::fs::write(f.remote("README.md"), "# readme\n").unwrap();
}

#[tokio::test]
async fn glob_and_grep_search_the_host_in_the_local_shape() {
    let f = fixture_or_skip!();
    seed_tree(&f);
    let found = f.call("glob", json!({"pattern": "**/*.rs"})).await.unwrap();
    let mut paths: Vec<&str> = found.content.lines().collect();
    paths.sort();
    assert_eq!(
        paths,
        ["src/lib.rs", "src/nested/mod.rs"],
        "{}",
        found.content
    );
    let hits = f
        .call(
            "grep",
            json!({"pattern": "TODO: (beta|delta)", "include": "*.rs"}),
        )
        .await
        .unwrap();
    assert!(
        hits.content
            .contains("src/lib.rs:\n  Line 2: // TODO: beta"),
        "{}",
        hits.content
    );
    assert!(
        hits.content
            .contains("src/nested/mod.rs:\n  Line 2: // TODO: delta"),
        "{}",
        hits.content
    );
    let none = f
        .call("grep", json!({"pattern": "no-such-text"}))
        .await
        .unwrap();
    assert_eq!(none.content, "No matches found");
}

#[tokio::test]
async fn git_runs_in_the_remote_repository() {
    let f = fixture_or_skip!();
    let root = f.server.remote_root();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@forge.invalid"],
        vec!["config", "user.name", "T"],
    ] {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(&args)
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(f.remote("tracked.txt"), "x\n").unwrap();
    let status = f
        .call(
            "git",
            json!({"subcommand": "status", "args": ["--porcelain"]}),
        )
        .await
        .unwrap();
    assert!(
        status.content.contains("?? tracked.txt"),
        "{}",
        status.content
    );
    f.call("git", json!({"subcommand": "add", "args": ["tracked.txt"]}))
        .await
        .unwrap();
    let commit = f
        .call(
            "git",
            json!({"subcommand": "commit", "args": ["-m", "first remote commit"]}),
        )
        .await
        .unwrap();
    assert!(!commit.is_error, "{}", commit.content);
    let log = std::process::Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["log", "--oneline"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&log.stdout).contains("first remote commit"));
    let refused = f
        .call("git", json!({"subcommand": "daemon", "args": []}))
        .await;
    assert!(
        refused.is_err(),
        "a subcommand off the allowlist is refused before anything runs"
    );
    assert!(f.local_workspace_is_empty());
}

// ---- Performance: round trips --------------------------------------------

/// Each SSH command costs several network round trips (channel open, exec,
/// output, exit status), so the number of them a tool makes is what a user
/// on a real network waits for. The wrapper `ssh` logs every invocation.
#[tokio::test]
async fn file_tools_make_at_most_two_round_trips() {
    let Some(server) = SshServer::start().await else {
        return;
    };
    let local = tempfile::tempdir().unwrap();
    let workspace = local.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let log = local.path().join("ssh.log");
    let target = Arc::new(SshTarget::new(
        forge_tools::target::testing::HOST,
        &server.remote_root().display().to_string(),
        &local.path().join("c"),
        server.counting_options(&log),
    ));
    target.connect().await.unwrap();
    let root = server.remote_root();
    std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(root.join("dot.png"), PNG).unwrap();
    let mut ctx = ToolContext::new(workspace).with_target(target.clone());
    ctx.image_input = true;
    let tools = default_builtins();
    let named = |name: &str| {
        tools
            .iter()
            .find(|tool| tool.name() == name)
            .unwrap()
            .clone()
    };
    for (name, args, most) in [
        ("read_file", json!({"path": "a.txt"}), 2),
        (
            "read_file",
            json!({"path": "a.txt", "offset": 2, "limit": 1}),
            2,
        ),
        ("ls", json!({}), 2),
        ("view_image", json!({"path": "dot.png"}), 2),
        ("write_file", json!({"path": "b.txt", "content": "x\n"}), 2),
        ("write_file", json!({"path": "b.txt", "content": "y\n"}), 2),
        (
            "edit",
            json!({"path": "b.txt", "old_string": "y", "new_string": "z"}),
            3,
        ),
        (
            "apply_patch",
            json!({"patch": "*** Begin Patch\n*** Add File: c.txt\n+c\n*** End Patch"}),
            2,
        ),
        ("glob", json!({"pattern": "*.txt"}), 1),
        ("grep", json!({"pattern": "two"}), 1),
        ("bash", json!({"command": "true"}), 1),
    ] {
        std::fs::write(&log, "").unwrap();
        let out = named(name).call(&ctx, args.clone()).await;
        assert!(
            out.as_ref().is_ok_and(|out| !out.is_error),
            "{name} {args}: {out:?}"
        );
        let calls = std::fs::read_to_string(&log).unwrap().lines().count();
        assert!(
            calls <= most,
            "{name} {args}: {calls} ssh commands, at most {most}"
        );
    }
    assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "z\n");
}

/// `glob` narrows the listing on the host (start folder, file name) instead
/// of fetching every file there; what it finds is unchanged.
#[tokio::test]
async fn glob_finds_the_same_files_when_narrowed_on_the_host() {
    let f = fixture_or_skip!();
    seed_tree(&f);
    std::fs::create_dir_all(f.remote("docs/nested")).unwrap();
    std::fs::write(f.remote("docs/nested/guide.md"), "# guide\n").unwrap();
    for (pattern, expected) in [
        ("src/nested/*.rs", vec!["src/nested/mod.rs"]),
        ("src/*.rs", vec!["src/lib.rs"]),
        ("**/*.md", vec!["README.md", "docs/nested/guide.md"]),
        ("*.md", vec!["README.md", "docs/nested/guide.md"]),
        ("nested", vec!["docs/nested/guide.md", "src/nested/mod.rs"]),
        ("no-such/*.rs", vec![]),
    ] {
        let out = f.call("glob", json!({ "pattern": pattern })).await.unwrap();
        let mut found: Vec<&str> = out
            .content
            .lines()
            .filter(|line| !line.is_empty())
            .collect();
        found.retain(|line| *line != "No files found");
        found.sort();
        assert_eq!(found, expected, "{pattern}: {}", out.content);
    }
    let escape = f.call("glob", json!({ "pattern": "../*" })).await;
    assert!(escape
        .map(|out| out.is_error || out.content == "No files found")
        .unwrap_or(true));
}

// ---- The file helper on the host -----------------------------------------

/// A fixture whose `ssh` logs each invocation, so a test can see when a file
/// operation opened a new SSH command and when it reused the helper.
struct Counted {
    server: SshServer,
    _local: tempfile::TempDir,
    log: PathBuf,
    target: Arc<SshTarget>,
    ctx: ToolContext,
}

impl Counted {
    async fn start(options: ServerOptions) -> Option<Self> {
        let server = SshServer::start_with(options).await?;
        let local = tempfile::tempdir().unwrap();
        let workspace = local.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let log = local.path().join("ssh.log");
        let target = Arc::new(SshTarget::new(
            forge_tools::target::testing::HOST,
            &server.remote_root().display().to_string(),
            &local.path().join("c"),
            server.counting_options(&log),
        ));
        let ctx = ToolContext::new(workspace).with_target(target.clone());
        Some(Self {
            server,
            _local: local,
            log,
            target,
            ctx,
        })
    }

    fn remote(&self, rel: &str) -> PathBuf {
        self.server.remote_root().join(rel)
    }

    async fn call(&self, tool: &str, args: Value) -> forge_types::ToolOutput {
        let out = tool_named(tool).call(&self.ctx, args.clone()).await;
        match out {
            Ok(out) if !out.is_error => out,
            other => panic!("{tool} {args}: {other:?}"),
        }
    }

    fn reset_log(&self) {
        std::fs::write(&self.log, "").unwrap();
    }

    fn ssh_commands(&self) -> usize {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .count()
    }

    /// The helper processes serving this session on the host.
    fn helpers_on_host(&self) -> usize {
        let temp = self.target.remote_temp().expect("connected");
        let out = std::process::Command::new("pgrep")
            .args(["-f", &format!("forge-helper --serve {temp}")])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).lines().count()
    }
}

/// A `bin` directory whose `grep` takes `seconds` before it searches: a slow
/// search, to run alongside other work.
fn slow_grep_bin(seconds: u32) -> tempfile::TempDir {
    let bin = tempfile::tempdir().unwrap();
    let grep = which("grep").expect("grep");
    let script = bin.path().join("grep");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nsleep {seconds}\nexec {} \"$@\"\n",
            grep.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

#[tokio::test]
async fn file_operations_reuse_a_helper_after_the_first() {
    let Some(c) = Counted::start(ServerOptions::default()).await else {
        return;
    };
    std::fs::write(c.remote("a.txt"), "one\ntwo\n").unwrap();
    c.call("read_file", json!({"path": "a.txt"})).await;
    c.reset_log();
    for i in 0..5 {
        assert_eq!(
            c.call("read_file", json!({"path": "a.txt"})).await.content,
            "one\ntwo"
        );
        c.call(
            "write_file",
            json!({"path": format!("w{i}.txt"), "content": "x\n"}),
        )
        .await;
        c.call("ls", json!({})).await;
        c.call("glob", json!({"pattern": "*.txt"})).await;
        c.call("grep", json!({"pattern": "two"})).await;
    }
    assert_eq!(
        c.ssh_commands(),
        0,
        "every file operation went to a running helper"
    );
    assert!(c.helpers_on_host() >= 1);
}

/// A slow search in one helper does not hold up a read: the pool serves it
/// from another.
#[tokio::test]
async fn a_slow_search_does_not_hold_up_a_read() {
    let bin = slow_grep_bin(3);
    let Some(c) = Counted::start(ServerOptions {
        path_prefix: vec![bin.path().to_path_buf()],
        ..Default::default()
    })
    .await
    else {
        return;
    };
    std::fs::write(c.remote("a.txt"), "hello\n").unwrap();
    c.call("read_file", json!({"path": "a.txt"})).await;
    let search = c.call("grep", json!({"pattern": "hello"}));
    let read = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let started = Instant::now();
        let out = c.call("read_file", json!({"path": "a.txt"})).await;
        (out, started.elapsed())
    };
    let (found, (out, took)) = tokio::join!(search, read);
    assert!(found.content.contains("a.txt"), "{}", found.content);
    assert_eq!(out.content, "hello");
    assert!(
        took < Duration::from_millis(2000),
        "the read waited for the search: {took:?}"
    );
}

/// A helper that dies on the host is replaced; a search abandoned midway
/// (its tool call dropped) never hands its late answer to the next caller.
#[tokio::test]
async fn a_lost_or_abandoned_helper_is_replaced_cleanly() {
    let bin = slow_grep_bin(2);
    let Some(c) = Counted::start(ServerOptions {
        path_prefix: vec![bin.path().to_path_buf()],
        ..Default::default()
    })
    .await
    else {
        return;
    };
    std::fs::write(c.remote("a.txt"), "alpha\n").unwrap();
    std::fs::write(c.remote("b.txt"), "beta\n").unwrap();
    c.call("read_file", json!({"path": "a.txt"})).await;
    assert!(c.helpers_on_host() >= 1, "a helper serves the session");

    let temp = c.target.remote_temp().unwrap();
    std::process::Command::new("pkill")
        .args(["-f", &format!("forge-helper --serve {temp}")])
        .status()
        .unwrap();
    assert_eq!(
        c.call("read_file", json!({"path": "b.txt"})).await.content,
        "beta"
    );

    let grep = tool_named("grep");
    let abandoned = tokio::time::timeout(
        Duration::from_millis(500),
        grep.call(&c.ctx, json!({"pattern": "alpha"})),
    )
    .await;
    assert!(abandoned.is_err(), "the search was still running");
    for _ in 0..4 {
        assert_eq!(
            c.call("read_file", json!({"path": "b.txt"})).await.content,
            "beta"
        );
    }
    c.reset_log();
    c.call("read_file", json!({"path": "a.txt"})).await;
    assert_eq!(c.ssh_commands(), 0, "healthy helpers are reused");
}

/// A host without `base64` cannot take a write through the helper: every
/// file operation still works, the way it did before helpers.
#[tokio::test]
async fn a_host_without_base64_still_reads_and_writes() {
    let empty = tempfile::tempdir().unwrap();
    let bin = empty.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for tool in [
        "bash", "uname", "mktemp", "cat", "mkdir", "rm", "kill", "sleep", "realpath", "head",
        "tail", "dirname", "stat", "chmod", "od", "wc", "find", "grep", "env", "ls",
    ] {
        if let Ok(found) = which(tool) {
            std::os::unix::fs::symlink(found, bin.join(tool)).unwrap();
        }
    }
    let Some(c) = Counted::start(ServerOptions {
        path_prefix: vec![bin.clone()],
        only_path: true,
        ..Default::default()
    })
    .await
    else {
        return;
    };
    c.call(
        "write_file",
        json!({"path": "n.txt", "content": "no base64\n"}),
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(c.remote("n.txt")).unwrap(),
        "no base64\n"
    );
    assert_eq!(
        c.call("read_file", json!({"path": "n.txt"})).await.content,
        "no base64"
    );
    c.call(
        "edit",
        json!({"path": "n.txt", "old_string": "no", "new_string": "still"}),
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(c.remote("n.txt")).unwrap(),
        "still base64\n"
    );
}

#[tokio::test]
async fn closing_the_session_stops_its_helpers() {
    let Some(c) = Counted::start(ServerOptions::default()).await else {
        return;
    };
    std::fs::write(c.remote("a.txt"), "a\n").unwrap();
    c.call("read_file", json!({"path": "a.txt"})).await;
    assert!(c.helpers_on_host() >= 1, "a helper serves the session");
    let temp = c.target.remote_temp().unwrap();
    c.target.close().await;
    let gone = wait_for(
        || {
            let out = std::process::Command::new("pgrep")
                .args(["-f", &format!("forge-helper --serve {temp}")])
                .output()
                .unwrap();
            out.stdout.is_empty()
        },
        Duration::from_secs(5),
    )
    .await;
    assert!(gone, "no helper is left running on the host");
}

/// A connection cut while a file operation waits on the helper is one clear
/// error; the next operation reconnects and works.
#[tokio::test]
async fn a_cut_connection_fails_the_waiting_operation_then_recovers() {
    let bin = slow_grep_bin(3);
    let Some(c) = Counted::start(ServerOptions {
        path_prefix: vec![bin.path().to_path_buf()],
        ..Default::default()
    })
    .await
    else {
        return;
    };
    std::fs::write(c.remote("a.txt"), "a\n").unwrap();
    c.call("read_file", json!({"path": "a.txt"})).await;
    let grep = tool_named("grep");
    let search = grep.call(&c.ctx, json!({"pattern": "a"}));
    let cut = async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        c.server.sever();
    };
    let (result, ()) = tokio::join!(search, cut);
    let error = match result {
        Err(error) => error.to_string(),
        Ok(out) => panic!("a cut connection must be an error: {}", out.content),
    };
    assert!(error.contains("forge-test"), "{error}");
    assert_eq!(
        c.call("read_file", json!({"path": "a.txt"})).await.content,
        "a"
    );
}

// ---- The file helper: audit fixes ------------------------------------------

/// The helper holds an operation's output on the host only up to what Forge
/// reads (64 MiB): reading a much larger file cannot fill the host's temp.
#[tokio::test]
async fn a_huge_read_never_piles_up_in_the_hosts_temp() {
    let Some(c) = Counted::start(ServerOptions::default()).await else {
        return;
    };
    std::fs::write(c.remote("a.txt"), "a\n").unwrap();
    c.call("read_file", json!({"path": "a.txt"})).await;
    let big = c.remote("big.bin");
    let file = std::fs::File::create(&big).unwrap();
    file.set_len(80 * 1024 * 1024).unwrap();
    let temp = PathBuf::from(c.target.remote_temp().unwrap());
    let watching = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let peak = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let watcher = {
        let (watching, peak, temp) = (watching.clone(), peak.clone(), temp.clone());
        std::thread::spawn(move || {
            while watching.load(std::sync::atomic::Ordering::SeqCst) {
                for entry in std::fs::read_dir(&temp).into_iter().flatten().flatten() {
                    if entry.file_name().to_string_lossy().starts_with("helper.") {
                        let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        peak.fetch_max(len, std::sync::atomic::Ordering::SeqCst);
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };
    // write_file reads the old content first, whole.
    let result = tool_named("write_file")
        .call(&c.ctx, json!({"path": "big.bin", "content": "small now\n"}))
        .await;
    watching.store(false, std::sync::atomic::Ordering::SeqCst);
    watcher.join().unwrap();
    let _ = result;
    let peak = peak.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        peak <= 64 * 1024 * 1024 + 64 * 1024,
        "the helper held {peak} bytes on the host"
    );
}

/// A request abandoned midway (its tool call dropped) stops on the host too,
/// like a command stopped with Stop.
#[tokio::test]
async fn an_abandoned_request_stops_on_the_host() {
    let bin = tempfile::tempdir().unwrap();
    let marker = bin.path().join("grep.pid");
    let grep = which("grep").expect("grep");
    let script = bin.path().join("grep");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho $$ > {}\nsleep 30\nexec {} \"$@\"\n",
            marker.display(),
            grep.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let Some(c) = Counted::start(ServerOptions {
        path_prefix: vec![bin.path().to_path_buf()],
        ..Default::default()
    })
    .await
    else {
        return;
    };
    std::fs::write(c.remote("a.txt"), "a\n").unwrap();
    c.call("read_file", json!({"path": "a.txt"})).await;
    let grep_tool = tool_named("grep");
    // Dropped once it is under way on the host (not after a fixed time,
    // which a busy machine may not have reached it by).
    let started = tokio::select! {
        _ = grep_tool.call(&c.ctx, json!({"pattern": "a"})) => false,
        started = wait_for(|| marker.exists(), Duration::from_secs(10)) => started,
    };
    assert!(
        started,
        "the slow search started on the host and was still running"
    );
    assert!(
        wait_for(|| !pid_alive(&marker), Duration::from_secs(5)).await,
        "the abandoned search was stopped on the host"
    );
}

/// Helpers beyond the first close once idle: each is an SSH session, and a
/// host allows only so many per connection (`MaxSessions`).
#[tokio::test]
async fn extra_helpers_give_their_sessions_back() {
    let bin = slow_grep_bin(1);
    let Some(server) = SshServer::start_with(ServerOptions {
        path_prefix: vec![bin.path().to_path_buf()],
        max_sessions: Some(3),
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let local = tempfile::tempdir().unwrap();
    let workspace = local.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let target = Arc::new(SshTarget::new(
        forge_tools::target::testing::HOST,
        &server.remote_root().display().to_string(),
        &local.path().join("c"),
        forge_tools::target::SshOptions {
            helper_idle: Duration::from_millis(500),
            ..server.options()
        },
    ));
    let ctx = ToolContext::new(workspace).with_target(target.clone());
    std::fs::write(server.remote_root().join("a.txt"), "a\n").unwrap();
    let grep = tool_named("grep");
    // Three searches at once: three helpers, every session the host allows.
    let (a, b, d) = tokio::join!(
        grep.call(&ctx, json!({"pattern": "a"})),
        grep.call(&ctx, json!({"pattern": "a"})),
        grep.call(&ctx, json!({"pattern": "a"})),
    );
    assert!(a.is_ok() && b.is_ok() && d.is_ok());
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let out = tool_named("bash")
        .call(&ctx, json!({"command": "echo room"}))
        .await
        .unwrap();
    assert_eq!(out.content.trim(), "room", "a command found a free session");
}

/// When the host allows no more sessions on the connection, `ssh` opens a
/// connection of its own for the command: it still runs, and nothing of
/// `ssh`'s own complaint ends up in what the command printed.
#[tokio::test]
async fn a_host_out_of_sessions_still_runs_a_command_cleanly() {
    let Some(f) = fixture_with(ServerOptions {
        max_sessions: Some(2),
        ..Default::default()
    })
    .await
    else {
        return;
    };
    std::fs::write(f.remote("a.txt"), "a\n").unwrap();
    // One session for the helper, one for a command left running.
    f.call("read_file", json!({"path": "a.txt"})).await.unwrap();
    let tools = default_builtins();
    let exec = tools
        .iter()
        .find(|tool| tool.name() == "exec_command")
        .unwrap();
    exec.call(&f.ctx, json!({"cmd": "sleep 30", "yield_time_ms": 200}))
        .await
        .unwrap();
    let out = f.call("bash", json!({"command": "echo hi"})).await.unwrap();
    assert_eq!(out.exit_code, Some(0), "{}", out.content);
    assert_eq!(out.content.trim(), "hi");
}
