//! End to end: `forge --continue` in a real PTY reopens the workspace's
//! *selected* session, not the most recent one.
#![cfg(unix)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use forge_session::{
    open_session, run_headless, ApprovalPolicy, NewRepositorySession, RepositoryControl,
    SessionTarget, WorktreeOwnership,
};

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

/// Where `dirs::config_dir()` resolves for a child given this HOME.
fn config_dir(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/forge")
    } else {
        home.join(".config/forge")
    }
}

async fn session_with_prompt(cfg: &forge_config::Config, prompt: &str) -> forge_types::SessionId {
    let opened = open_session(cfg, SessionTarget::New).await.unwrap();
    let session_id = opened.session.session_id;
    // The mock model answers; the user's prompt is what lands on screen.
    let _ = run_headless(opened.session, prompt, ApprovalPolicy::DenyAll).await;
    session_id
}

async fn wait_closed(journal: &Path) {
    let wal = journal.with_extension("db-wal");
    let shm = journal.with_extension("db-shm");
    let deadline = Instant::now() + Duration::from_secs(10);
    while wal.exists() || shm.exists() {
        assert!(Instant::now() < deadline, "journal never closed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn continue_reopens_the_selected_session_in_the_tui() {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};

    let repo = git_repo();
    let mut cfg = forge_config::Config {
        resolved_workspace: repo.path().to_path_buf(),
        workspace_root: Some(repo.path().display().to_string()),
        ..Default::default()
    };
    cfg.model.mock = true;
    let (journal_dir, _) = forge_session::resolve_journal_dir(&cfg);
    cfg.journal.path = journal_dir.display().to_string();

    let selected = session_with_prompt(&cfg, "SELECTEDMARKER please").await;
    let recent = session_with_prompt(&cfg, "RECENTMARKER please").await;
    for id in [selected, recent] {
        wait_closed(&journal_dir.join(format!("{id}.db"))).await;
    }
    for (id, age) in [(selected, 3600), (recent, 0)] {
        std::fs::File::options()
            .write(true)
            .open(journal_dir.join(format!("{id}.db")))
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(age))
            .unwrap();
    }
    let control = RepositoryControl::open(&repo.path().join(".forge/local/control"))
        .await
        .unwrap();
    for (id, label, workspace, ownership) in [
        (
            selected,
            "selected",
            repo.path().to_path_buf(),
            WorktreeOwnership::Primary,
        ),
        (
            recent,
            "recent",
            repo.path().join("other"),
            WorktreeOwnership::Attached,
        ),
    ] {
        std::fs::create_dir_all(&workspace).unwrap();
        control
            .register_session(
                NewRepositorySession {
                    session_id: id,
                    label: label.into(),
                    workspace,
                    branch: "main".into(),
                    ownership,
                    slot: None,
                    model_id: "mock".into(),
                    profile_id: String::new(),
                    reasoning_effort: None,
                },
                None,
            )
            .await
            .unwrap();
    }
    control.set_selected(Some(selected)).await.unwrap();
    drop(control);

    // A HOME where the theme is committed and the workspace trusted, so the
    // TUI skips its setup screens and goes straight to the session.
    let home = tempfile::tempdir().unwrap();
    let config = config_dir(home.path());
    std::fs::create_dir_all(&config).unwrap();
    forge_config::persist_committed_theme_at(
        &config.join("config.toml"),
        forge_config::DEFAULT_THEME_ID,
    )
    .unwrap();
    forge_config::grant_trust_at(&config.join("trust.toml"), repo.path()).unwrap();

    let pty = match native_pty_system().openpty(PtySize {
        rows: 40,
        cols: 140,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(pty) => pty,
        Err(error) if error.to_string().contains("Operation not permitted") => {
            eprintln!("PTY unavailable in this environment: {error}");
            return;
        }
        Err(error) => panic!("open PTY: {error}"),
    };
    let mut command = CommandBuilder::new(std::env::var("CARGO_BIN_EXE_forge").unwrap());
    command.arg("--continue");
    command.cwd(repo.path());
    command.env("HOME", home.path());
    command.env("XDG_CONFIG_HOME", home.path().join(".config"));
    command.env("XDG_DATA_HOME", home.path().join(".local/share"));
    command.env("TERM", "xterm-256color");
    let mut child = pty.slave.spawn_command(command).unwrap();
    drop(pty.slave);

    let mut reader = pty.master.try_clone_reader().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        while let Ok(read) = reader.read(&mut buffer) {
            if read == 0 || sender.send(buffer[..read].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut screen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Ok(chunk) = receiver.recv_timeout(Duration::from_millis(100)) {
            screen.extend(chunk);
        }
        if String::from_utf8_lossy(&screen).contains("SELECTEDMARKER") {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    let text = String::from_utf8_lossy(&screen);
    assert!(
        text.contains("SELECTEDMARKER"),
        "the selected session's transcript must be on screen; got {} bytes",
        screen.len()
    );
    assert!(
        !text.contains("RECENTMARKER"),
        "the most recent (unselected) session must not be the one opened"
    );
}
