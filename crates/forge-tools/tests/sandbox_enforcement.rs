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

#![cfg(target_os = "macos")]

use std::path::Path;
use std::process::Command;

use forge_tools::sandbox::{availability, wrap_shell_command, SandboxPolicy};

/// Run `command` confined to `root`, returning (exit_ok, stdout+stderr).
fn run_confined(root: &Path, command: &str) -> (bool, String) {
    let policy = SandboxPolicy::for_workspace(root);
    let (program, args) =
        wrap_shell_command("/bin/bash", command, &policy).expect("sandbox should be available");
    let out = Command::new(program)
        .args(args)
        .current_dir(root)
        .output()
        .expect("spawn sandbox-exec");
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

#[test]
fn sandbox_is_available_on_macos() {
    assert!(availability().is_ok(), "macOS hosts must have sandbox-exec");
}

#[test]
fn writes_inside_the_workspace_succeed() {
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
    let ws = workspace();
    std::fs::create_dir_all(ws.path().join("src")).unwrap();
    std::fs::write(ws.path().join("src/main.rs"), "fn main() {}").unwrap();
    let (ok, out) = run_confined(ws.path(), "rm -rf src");
    assert!(ok, "workspace-write permits this by design: {out}");
    assert!(!ws.path().join("src").exists());
}

#[test]
fn git_directory_is_read_only() {
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
    let ws = workspace();
    let (ok, _) = run_confined(ws.path(), "curl -s -m 5 https://example.com");
    assert!(!ok, "network egress must be denied");
}

/// Reads outside the workspace are permitted — toolchains need `~/.gitconfig`
/// and `~/.cargo`. Pinned so the choice is visible rather than assumed: a
/// secret can be read, but the network denial is what stops it leaving.
#[test]
fn reads_outside_the_workspace_are_permitted_by_design() {
    let ws = workspace();
    let (ok, out) = run_confined(ws.path(), "head -c 5 /etc/hosts > read.txt && echo READ");
    assert!(ok, "broad reads are intentional: {out}");
    assert!(ws.path().join("read.txt").exists());
}

/// The shapes `readonly.rs` used to gate by parsing. None of them are
/// recognised here — they simply cannot reach anything.
#[test]
fn commands_that_used_to_need_parsing_are_contained_instead() {
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
