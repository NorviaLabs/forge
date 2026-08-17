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

    let out = run_shell_command(&format!("echo pwned > {target}"), ws.path())
        .await
        .expect("the tool itself must not error");
    assert!(
        out.is_error,
        "escaping the workspace must fail: {:?}",
        out.content
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
    let out = run_shell_command("echo clobbered > .git/HEAD", ws.path())
        .await
        .unwrap();
    assert!(out.is_error, "the recovery mechanism must stay read-only");
    assert_eq!(
        std::fs::read_to_string(ws.path().join(".git/HEAD")).unwrap(),
        "ref: refs/heads/main\n"
    );
}
