//! Signed commits from a confined git, end to end: real git, the real
//! `forge` binary as git's signing program, and a real `ssh-keygen` run on
//! the host by the signing broker. Nothing leaves the machine.
//!
//! Its own test binary, because it points `GIT_CONFIG_GLOBAL` at a scratch
//! "user" config for the whole process.
#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use forge_tools::egress::{EgressOptions, EgressPolicy, EgressProxy};
use forge_tools::sandbox::git as confined_git;
use forge_tools::sandbox::EgressGrant;

fn run(program: &str, dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn ok(program: &str, dir: &Path, args: &[&str]) -> String {
    let out = run(program, dir, args);
    assert!(out.status.success(), "{program} {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn keygen(dir: &Path, name: &str) -> PathBuf {
    let key = dir.join(name);
    ok(
        "ssh-keygen",
        dir,
        &[
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            name,
            "-f",
            key.to_str().unwrap(),
        ],
    );
    key
}

#[test]
fn a_confined_commit_is_signed_on_the_host_with_the_users_own_key() {
    if forge_tools::sandbox::availability().is_err() {
        return;
    }
    // The user's side: a key and a git config that signs with it.
    // Somewhere a confined process may read, like the real `~/.gitconfig`;
    // the system temp directory is not.
    let user = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let user_dir = user.path().canonicalize().unwrap();
    let user_key = keygen(&user_dir, "user_key");
    let other_key = keygen(&user_dir, "other_key");
    let global = user_dir.join("gitconfig");
    std::fs::write(
        &global,
        format!(
            "[user]\n\tname = Tester\n\temail = tester@example.test\n\tsigningkey = {}\n\
             [gpg]\n\tformat = ssh\n[commit]\n\tgpgsign = true\n",
            user_key.display()
        ),
    )
    .unwrap();
    // Before any thread exists that could read the environment.
    std::env::set_var("GIT_CONFIG_GLOBAL", &global);
    std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    forge_tools::set_egress_connect_exe(PathBuf::from(env!("CARGO_BIN_EXE_forge")));

    // The repository's side, which a confined process can write: it names
    // another program and another key.
    let ws = tempfile::tempdir().unwrap();
    let repo = ws.path().canonicalize().unwrap();
    ok("git", &repo, &["init", "-q", "-b", "main"]);
    let marker = repo.join("planted-program-ran");
    let planted = repo.join("planted-signer");
    std::fs::write(
        &planted,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    )
    .unwrap();
    ok("chmod", &repo, &["+x", planted.to_str().unwrap()]);
    ok(
        "git",
        &repo,
        &["config", "gpg.ssh.program", planted.to_str().unwrap()],
    );
    ok(
        "git",
        &repo,
        &["config", "user.signingkey", other_key.to_str().unwrap()],
    );
    std::fs::write(repo.join("file.txt"), "hello\n").unwrap();
    ok("git", &repo, &["add", "file.txt"]);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    forge_tools::install_git_network(runtime.handle().clone());
    let proxy = runtime
        .block_on(EgressProxy::start_with_options(
            EgressPolicy::new(),
            EgressOptions::default(),
        ))
        .unwrap();
    let grant = Arc::new(EgressGrant {
        proxy_port: proxy.addr().port(),
        socket_path: PathBuf::new(),
        control: Some(proxy.shared().clone()),
    });
    let _registered = forge_tools::register_workspace_egress(&repo, Arc::clone(&grant));

    let mut git = confined_git::command_networked(&repo, &[]);
    assert!(git.confined);
    let out = git
        .command
        .args(["commit", "-q", "-m", "signed"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    drop(git);

    assert!(!marker.exists(), "the repository's signing program ran");
    let commit = ok("git", &repo, &["cat-file", "commit", "HEAD"]);
    assert!(
        commit.contains("gpgsig -----BEGIN SSH SIGNATURE-----"),
        "{commit}"
    );

    // The signature is the user's key's, not the one the repository named.
    let allowed = user_dir.join("allowed_signers");
    let public = std::fs::read_to_string(user_key.with_extension("pub")).unwrap();
    std::fs::write(&allowed, format!("tester@example.test {public}")).unwrap();
    let verify = run(
        "git",
        &repo,
        &[
            "-c",
            &format!("gpg.ssh.allowedSignersFile={}", allowed.display()),
            "-c",
            "gpg.ssh.program=ssh-keygen",
            "verify-commit",
            "HEAD",
        ],
    );
    assert!(verify.status.success(), "{verify:?}");

    // The agent's `git` tool takes the same route: confined, with the
    // user's identity and signature.
    use forge_tools::Tool as _;
    std::fs::write(repo.join("file.txt"), "hello again\n").unwrap();
    let session_tmp = forge_tools::SessionTempDir::create("git-signing-test").unwrap();
    let mut ctx = forge_tools::ToolContext::new(repo.clone())
        .with_tool_call_id("call-sign")
        .with_session_tmp(session_tmp);
    ctx.egress = Some(grant);
    let out = runtime
        .block_on(forge_tools::GitTool.call(
            &ctx,
            serde_json::json!({"subcommand": "commit", "args": ["-a", "-m", "by the agent"]}),
        ))
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(!marker.exists(), "the repository's signing program ran");
    let commit = ok("git", &repo, &["cat-file", "commit", "HEAD"]);
    assert!(commit.contains("by the agent"), "{commit}");
    assert!(
        commit.contains("author Tester <tester@example.test>"),
        "{commit}"
    );
    assert!(
        commit.contains("gpgsig -----BEGIN SSH SIGNATURE-----"),
        "{commit}"
    );
}
