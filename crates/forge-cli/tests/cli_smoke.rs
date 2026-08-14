use assert_cmd::Command;

#[test]
fn help_lists_only_core_options() {
    let assert = Command::cargo_bin("forge")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout);

    // The default command launches the TUI; help should not advertise
    // subcommands. Clap collects every subcommand under a `Commands:`
    // heading, so asserting the heading is absent covers all of them at
    // once — and unlike the bare words this used to check for ("run",
    // "connect", "approve"), it cannot be tripped by an option's prose.
    assert!(
        !out.contains("Commands:"),
        "help must not advertise subcommands:\n{out}"
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
