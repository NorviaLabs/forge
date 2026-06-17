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
        .stdout(predicate::str::contains("forge 0.3.0"));
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
