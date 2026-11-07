//! `forge` refuses to start on a repository another Forge process owns:
//! exit code 1, the owner named on stderr, before any setup screen, and
//! without touching the owner's state.

use assert_cmd::Command;

fn git_repo() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("tracked.txt"), "one\n").unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "forge@example.test"][..],
        &["config", "user.name", "Forge Test"][..],
        &["add", "tracked.txt"][..],
        &["commit", "-q", "-m", "init"][..],
    ] {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(temp.path())
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    temp
}

/// Every file under `dir` with its bytes.
fn file_list(dir: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap_or_default();
                files.push((path, bytes));
            }
        }
    }
    files.sort();
    files
}

#[tokio::test]
async fn forge_on_an_owned_repository_exits_1_naming_the_owner_before_setup() {
    let repo = git_repo();
    let cfg = forge_config::Config {
        resolved_workspace: repo.path().to_path_buf(),
        workspace_root: Some(repo.path().display().to_string()),
        ..Default::default()
    };
    // This test process is the owner; the binary is a second process.
    let owner = forge_session::RepositoryBootstrap::acquire(&cfg)
        .await
        .expect("the test takes the lease");
    let before = file_list(&repo.path().join(".forge"));
    // A fresh HOME has no committed theme and no trust: before S2's
    // ownership-first ordering this launch would have opened the setup
    // screen instead of refusing.
    let home = tempfile::tempdir().unwrap();

    let assert = Command::cargo_bin("forge")
        .unwrap()
        .arg("--tui")
        .current_dir(repo.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_DATA_HOME", home.path().join(".local/share"))
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .code(1);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        stderr.contains("already owns this repository"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains(&format!("pid {},", std::process::id())),
        "stderr must name the owning process: {stderr}"
    );
    // Compared byte for byte while the owner (this process) is still alive.
    assert!(
        file_list(&repo.path().join(".forge")) == before,
        "a refused launch must not create or rewrite session or control files"
    );
    let home_files: Vec<_> = file_list(home.path())
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert!(
        home_files.is_empty(),
        "a refused launch must not write trust, theme or other user state: {home_files:?}"
    );
    drop(owner);
}
