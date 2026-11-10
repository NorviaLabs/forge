//! Find and start the desktop frontend, the default `forge` launch.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const INSTALL_HINT: &str = "Forge desktop isn't installed. Re-run the Forge installer to add it:\n\
     curl -fsSL https://raw.githubusercontent.com/NorviaLabs/forge/main/install/forge-installer.sh | sh";

/// Run the desktop app and return its exit code. `None` opens the current
/// directory, which is what a bare `forge` should do.
pub fn run(workspace: Option<&Path>) -> i32 {
    let Some(binary) = binary() else {
        eprintln!("{INSTALL_HINT}");
        return 1;
    };
    let mut command = Command::new(binary);
    if let Some(workspace) = workspace {
        command.arg(workspace);
    }
    match command.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!("forge: could not start the desktop app: {error}");
            1
        }
    }
}

/// The GUI needs a desktop session. Linux is the only platform where the
/// session can be absent while the CLI still runs.
pub fn display_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|name| env::var_os(name).is_some_and(|value| !value.is_empty()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

fn binary() -> Option<PathBuf> {
    if let Some(path) = env::var_os("FORGE_DESKTOP_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(directory) = env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        if let Some(found) = in_directory(&directory) {
            return Some(found);
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(found) = mac_app() {
        return Some(found);
    }
    let path = env::var_os("PATH")?;
    env::split_paths(&path).find_map(|directory| in_directory(&directory))
}

fn in_directory(directory: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "forge-desktop.exe"
    } else {
        "forge-desktop"
    };
    let candidate = directory.join(name);
    candidate.is_file().then_some(candidate)
}

#[cfg(target_os = "macos")]
fn mac_app() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = env::var_os("HOME") {
        candidates
            .push(PathBuf::from(home).join("Applications/Forge.app/Contents/MacOS/forge-desktop"));
    }
    candidates.push(PathBuf::from(
        "/Applications/Forge.app/Contents/MacOS/forge-desktop",
    ));
    candidates.into_iter().find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("forge-launch-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_desktop_binary_in_a_directory_is_found() {
        let dir = tempdir("found");
        let name = if cfg!(windows) {
            "forge-desktop.exe"
        } else {
            "forge-desktop"
        };
        std::fs::write(dir.join(name), "").unwrap();
        assert_eq!(in_directory(&dir), Some(dir.join(name)));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn an_empty_directory_is_not_a_match() {
        let dir = tempdir("empty");
        assert_eq!(in_directory(&dir), None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_install_hint_names_the_installer() {
        assert!(INSTALL_HINT.contains("forge-installer.sh"));
    }
}
