use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

#[test]
fn status_exits_zero() {
    Command::cargo_bin("forge")
        .unwrap()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("forge "));
}

#[test]
fn help_lists_only_core_commands() {
    let assert = Command::cargo_bin("forge")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(out.contains("run"));
    assert!(out.contains("status"));
    assert!(out.contains("connect"));
    assert!(!out.contains("repl"));
    assert!(!out.contains("feedback"));
    assert!(!out.contains("channel"));
    assert!(!out.contains("fleet"));
    assert!(!out.contains("approve"));
    assert!(!out.contains("--provider"));
    assert!(!out.contains("--mock"));
}

#[test]
fn run_with_provider_mock_via_env() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("forge")
        .unwrap()
        .env("FORGE_MODEL_PROVIDER", "mock")
        .args([
            "--workspace",
            dir.path().to_str().unwrap(),
            "run",
            "hello",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("session_id="));
}

#[test]
fn connect_list_shows_profiles() {
    Command::cargo_bin("forge")
        .unwrap()
        .args(["connect", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("xai"))
        .stdout(predicate::str::contains("opencode_go"));
}

#[test]
fn connect_xai_rejects_api_key() {
    let dir = tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".config/forge")).unwrap();
    Command::cargo_bin("forge")
        .unwrap()
        .env("HOME", &home)
        .args(["connect", "xai", "--key", "super-secret-xai-key"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("OAuth"))
        .stderr(predicate::str::contains("super-secret-xai-key").not());
}

#[test]
fn connect_xai_oauth_fixture() {
    let dir = tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".config/forge")).unwrap();
    Command::cargo_bin("forge")
        .unwrap()
        .env("HOME", &home)
        .env("FORGE_CONNECT_OAUTH_FIXTURE", "1")
        .args(["connect", "xai"])
        .assert()
        .success()
        .stdout(predicate::str::contains("xAI Grok"))
        .stdout(predicate::str::contains("oauth"))
        .stdout(predicate::str::contains("fixture-access-token").not());
}

#[test]
fn connect_opencode_go_with_key_no_secret_leak() {
    let dir = tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".config/forge")).unwrap();
    Command::cargo_bin("forge")
        .unwrap()
        .env("HOME", &home)
        .args(["connect", "opencode_go", "--key", "go-secret-key"])
        .assert()
        .success()
        .stdout(predicate::str::contains("OpenCode Go"))
        .stdout(predicate::str::contains("go-secret-key").not());
}
