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
fn run_mock_completes() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("forge")
        .unwrap()
        .args([
            "--workspace",
            dir.path().to_str().unwrap(),
            "--mock",
            "run",
            "hello",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("session_id="));
}

#[test]
fn feedback_sensor_ok() {
    Command::cargo_bin("forge")
        .unwrap()
        .args(["feedback", "--sensor", "echo ok"])
        .assert()
        .success()
        .stdout(predicate::str::contains("passed=true"));
}

#[test]
fn channel_restricts_tools() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("forge")
        .unwrap()
        .args([
            "--workspace",
            dir.path().to_str().unwrap(),
            "--mock",
            "channel",
            "--kind",
            "webhook",
            "hello",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("tools_visible="));
}

#[test]
fn fleet_plugins_load() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("forge")
        .unwrap()
        .args([
            "--workspace",
            dir.path().to_str().unwrap(),
            "fleet",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("plugins="));
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
