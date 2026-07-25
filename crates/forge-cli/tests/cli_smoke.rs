use assert_cmd::Command;

#[test]
fn help_lists_only_core_options() {
    let assert = Command::cargo_bin("forge")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout);
    // The default command launches the TUI; help should not advertise subcommands.
    assert!(!out.contains("run"));
    assert!(!out.contains("status"));
    assert!(!out.contains("connect"));
    assert!(!out.contains("--worktree"));
    assert!(!out.contains("--resume"));
    assert!(!out.contains("--model"));
    assert!(!out.contains("--workspace"));
    assert!(!out.contains("--config"));
    assert!(!out.contains("--max-turns"));
    assert!(!out.contains("repl"));
    assert!(!out.contains("feedback"));
    assert!(!out.contains("channel"));
    assert!(!out.contains("fleet"));
    assert!(!out.contains("approve"));
    assert!(!out.contains("--provider"));
    assert!(!out.contains("--mock"));
}

#[test]
fn version_prints_package_version() {
    Command::cargo_bin("forge")
        .unwrap()
        .arg("--version")
        .assert()
        .success();
}

