use assert_cmd::Command;

#[test]
fn help_lists_core_options_and_native_bench_command() {
    let assert = Command::cargo_bin("forge")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout);

    // The default command launches the TUI, while `bench` is the supported
    // frontend-independent entry point for automation.
    assert!(
        out.contains("Commands:") && out.contains("bench"),
        "help must advertise the native bench command:\n{out}"
    );

    // Legacy flags that belonged to the removed subcommands.
    for flag in [
        "--worktree",
        "--model",
        "--workspace",
        "--config",
        "--max-turns",
        "--provider",
        "--mock",
        "--print",
        "--approvals",
    ] {
        assert!(
            !out.contains(flag),
            "`{flag}` should not be offered:\n{out}"
        );
    }

    assert!(
        out.contains("--resume"),
        "`--resume` should be offered:\n{out}"
    );
    for flag in ["--print", "--approvals"] {
        assert!(
            !out.contains(flag),
            "`{flag}` should not be offered:\n{out}"
        );
    }
}

#[test]
fn bench_help_lists_model_and_approval_controls() {
    let assert = Command::cargo_bin("forge")
        .unwrap()
        .args(["bench", "--help"])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(out.contains("--model"), "bench help needs --model:\n{out}");
    assert!(
        out.contains("--approve-all"),
        "bench help needs explicit approval policy:\n{out}"
    );
}

#[test]
fn version_prints_package_version() {
    Command::cargo_bin("forge")
        .unwrap()
        .arg("--version")
        .assert()
        .success();
}

#[test]
fn invalid_resume_id_fails_parsing() {
    Command::cargo_bin("forge")
        .unwrap()
        .args(["--resume", "not-a-uuid"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("invalid value"));
}
