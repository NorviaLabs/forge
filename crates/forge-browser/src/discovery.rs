//! Finding an installed Chromium-family browser.
//!
//! Forge does not ship or download a browser. It uses one the user already
//! has: the program named in `[browser] executable`, or the first known
//! Chromium-family program found on `PATH`. The candidate table is per
//! operating system, so adding macOS is a second table, not new logic.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

/// A browser Forge knows how to drive, by display name and program name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserCandidate {
    pub name: &'static str,
    pub program: &'static str,
}

/// Linux candidates, in order of preference.
pub const LINUX_CANDIDATES: &[BrowserCandidate] = &[
    BrowserCandidate {
        name: "Google Chrome",
        program: "google-chrome",
    },
    BrowserCandidate {
        name: "Google Chrome",
        program: "google-chrome-stable",
    },
    BrowserCandidate {
        name: "Chromium",
        program: "chromium",
    },
    BrowserCandidate {
        name: "Chromium",
        program: "chromium-browser",
    },
    BrowserCandidate {
        name: "Brave",
        program: "brave-browser",
    },
    BrowserCandidate {
        name: "Microsoft Edge",
        program: "microsoft-edge",
    },
    BrowserCandidate {
        name: "Microsoft Edge",
        program: "microsoft-edge-stable",
    },
    BrowserCandidate {
        name: "Vivaldi",
        program: "vivaldi",
    },
];

/// The candidates for the running operating system.
pub fn candidates() -> &'static [BrowserCandidate] {
    if cfg!(target_os = "linux") {
        LINUX_CANDIDATES
    } else {
        // macOS and Windows look in their application folders; not yet.
        &[]
    }
}

/// A browser that was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FoundBrowser {
    /// Display name ("Google Chrome"), or the file name for a configured
    /// program Forge does not recognise.
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DiscoveryError {
    #[error("the configured browser `{0}` was not found or is not executable")]
    ConfiguredMissing(String),
    #[error("no Chrome-family browser found; install one of: {0}")]
    NoneFound(String),
}

/// Find the browser to drive: `configured` when set (a path, or a program
/// name looked up on `path_var`), otherwise the first candidate on
/// `path_var`.
pub fn discover(
    configured: Option<&str>,
    path_var: Option<&OsStr>,
) -> Result<FoundBrowser, DiscoveryError> {
    if let Some(configured) = configured.map(str::trim).filter(|value| !value.is_empty()) {
        let as_path = Path::new(configured);
        let found = if as_path.is_absolute() {
            is_executable(as_path).then(|| as_path.to_path_buf())
        } else {
            find_on_path(configured, path_var)
        };
        return found
            .map(|path| FoundBrowser {
                name: display_name(&path),
                path,
            })
            .ok_or_else(|| DiscoveryError::ConfiguredMissing(configured.to_string()));
    }
    discover_all(path_var)
        .into_iter()
        .next()
        .ok_or_else(|| DiscoveryError::NoneFound(install_hint()))
}

/// `Google Chrome, Chromium, Brave, …` without repeats.
fn install_hint() -> String {
    let mut names: Vec<&str> = Vec::new();
    for candidate in LINUX_CANDIDATES {
        if !names.contains(&candidate.name) {
            names.push(candidate.name);
        }
    }
    names.join(", ")
}

/// The display name of a browser program: the candidate's name when the
/// file is a known program, otherwise its file name.
fn display_name(path: &Path) -> String {
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    candidates()
        .iter()
        .find(|candidate| candidate.program == file)
        .map(|candidate| candidate.name.to_string())
        .unwrap_or(file)
}

fn find_on_path(program: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path_var?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

/// Every candidate found on `path_var`, one entry per distinct program
/// path, for the settings screen.
pub fn discover_all(path_var: Option<&OsStr>) -> Vec<FoundBrowser> {
    let mut found: Vec<FoundBrowser> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for candidate in candidates() {
        let Some(path) = find_on_path(candidate.program, path_var) else {
            continue;
        };
        // `chromium-browser` is often a link to `chromium`: list it once.
        let real = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if seen.contains(&real) {
            continue;
        }
        seen.push(real);
        found.push(FoundBrowser {
            name: candidate.name.to_string(),
            path,
        });
    }
    found
}

/// Whether `path` is an executable regular file.
pub(crate) fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(target_os = "linux")]
    use tempfile::tempdir;

    #[cfg(target_os = "linux")]
    fn fake_program(dir: &Path, name: &str, executable: bool) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn linux_prefers_chrome_then_chromium_then_the_rest() {
        let programs: Vec<&str> = LINUX_CANDIDATES.iter().map(|c| c.program).collect();
        assert_eq!(programs[0], "google-chrome");
        assert!(programs.contains(&"brave-browser"));
        assert!(programs.contains(&"microsoft-edge"));
        assert!(programs.contains(&"vivaldi"));
        if cfg!(target_os = "linux") {
            assert_eq!(candidates(), LINUX_CANDIDATES);
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn the_first_candidate_on_path_wins() {
        let a = tempdir().unwrap();
        let b = tempdir().unwrap();
        fake_program(a.path(), "brave-browser", true);
        let chromium = fake_program(b.path(), "chromium", true);
        let path = std::env::join_paths([a.path(), b.path()]).unwrap();

        let found = discover(None, Some(&path)).unwrap();
        assert_eq!(found.path, chromium, "chromium ranks above brave");
        assert_eq!(found.name, "Chromium");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_file_that_is_not_executable_is_skipped() {
        let dir = tempdir().unwrap();
        fake_program(dir.path(), "google-chrome", false);
        let brave = fake_program(dir.path(), "brave-browser", true);
        let path = std::env::join_paths([dir.path()]).unwrap();
        assert_eq!(discover(None, Some(&path)).unwrap().path, brave);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn the_configured_program_wins_as_a_path_or_a_name() {
        let dir = tempdir().unwrap();
        fake_program(dir.path(), "google-chrome", true);
        let custom = fake_program(dir.path(), "my-browser", true);
        let path = std::env::join_paths([dir.path()]).unwrap();

        let by_path = discover(Some(custom.to_str().unwrap()), Some(&path)).unwrap();
        assert_eq!(by_path.path, custom);
        assert_eq!(by_path.name, "my-browser");

        let by_name = discover(Some("my-browser"), Some(&path)).unwrap();
        assert_eq!(by_name.path, custom);

        let known = discover(Some("google-chrome"), Some(&path)).unwrap();
        assert_eq!(known.name, "Google Chrome");
    }

    /// A configured browser that is missing is an error, not a silent
    /// fallback to some other browser.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_missing_configured_program_is_an_error() {
        let dir = tempdir().unwrap();
        fake_program(dir.path(), "google-chrome", true);
        let path = std::env::join_paths([dir.path()]).unwrap();
        assert_eq!(
            discover(Some("/nowhere/chrome"), Some(&path)),
            Err(DiscoveryError::ConfiguredMissing("/nowhere/chrome".into()))
        );
        assert!(
            discover(Some("  "), Some(&path)).is_ok(),
            "blank means unset"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn nothing_found_names_what_to_install() {
        let dir = tempdir().unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();
        let error = discover(None, Some(&path)).unwrap_err();
        let text = error.to_string();
        assert!(
            text.contains("Google Chrome") && text.contains("Brave"),
            "{text}"
        );
        assert!(discover(None, None).is_err());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn every_distinct_browser_is_listed_for_settings() {
        let dir = tempdir().unwrap();
        fake_program(dir.path(), "chromium", true);
        fake_program(dir.path(), "brave-browser", true);
        std::os::unix::fs::symlink(
            dir.path().join("chromium"),
            dir.path().join("chromium-browser"),
        )
        .unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();
        let names: Vec<String> = discover_all(Some(&path))
            .into_iter()
            .map(|b| b.name)
            .collect();
        assert_eq!(
            names,
            ["Chromium", "Brave"],
            "a symlink to the same program is listed once"
        );
    }
}
