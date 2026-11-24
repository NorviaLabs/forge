//! Finding an installed Chromium-family browser.
//!
//! Forge does not ship or download a browser. It uses one the user already
//! has: the program named in `[browser] executable`, or the first known
//! Chromium-family program it can find. A candidate says where to look —
//! a name on `PATH`, or the executable inside an application bundle — so
//! macOS is another table, not a different search.
//!
//! What comes back is the program launch execs. On macOS that is the binary
//! inside the bundle (`Contents/MacOS/…`), never the `.app` directory and
//! never `open`: launch attaches the DevTools pipes to the process it starts,
//! and Launch Services would drop them.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

/// Where a candidate is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locate {
    /// A program of this name on `PATH`.
    OnPath(&'static str),
    /// This path relative to an applications directory. The path is the
    /// executable inside the bundle, not the bundle itself.
    Application(&'static str),
}

/// A browser Forge knows how to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserCandidate {
    pub name: &'static str,
    pub locate: Locate,
}

/// Linux candidates, in order of preference.
pub const LINUX_CANDIDATES: &[BrowserCandidate] = &[
    BrowserCandidate {
        name: "Google Chrome",
        locate: Locate::OnPath("google-chrome"),
    },
    BrowserCandidate {
        name: "Google Chrome",
        locate: Locate::OnPath("google-chrome-stable"),
    },
    BrowserCandidate {
        name: "Chromium",
        locate: Locate::OnPath("chromium"),
    },
    BrowserCandidate {
        name: "Chromium",
        locate: Locate::OnPath("chromium-browser"),
    },
    BrowserCandidate {
        name: "Brave",
        locate: Locate::OnPath("brave-browser"),
    },
    BrowserCandidate {
        name: "Microsoft Edge",
        locate: Locate::OnPath("microsoft-edge"),
    },
    BrowserCandidate {
        name: "Microsoft Edge",
        locate: Locate::OnPath("microsoft-edge-stable"),
    },
    BrowserCandidate {
        name: "Vivaldi",
        locate: Locate::OnPath("vivaldi"),
    },
];

/// macOS candidates. Bundles come before `PATH`: a Homebrew shim must not
/// beat the executable whose process keeps the DevTools pipes.
pub const MACOS_CANDIDATES: &[BrowserCandidate] = &[
    BrowserCandidate {
        name: "Google Chrome",
        locate: Locate::Application("Google Chrome.app/Contents/MacOS/Google Chrome"),
    },
    BrowserCandidate {
        name: "Chromium",
        locate: Locate::Application("Chromium.app/Contents/MacOS/Chromium"),
    },
    BrowserCandidate {
        name: "Brave Browser",
        locate: Locate::Application("Brave Browser.app/Contents/MacOS/Brave Browser"),
    },
    BrowserCandidate {
        name: "Microsoft Edge",
        locate: Locate::Application("Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
    },
    BrowserCandidate {
        name: "Vivaldi",
        locate: Locate::Application("Vivaldi.app/Contents/MacOS/Vivaldi"),
    },
    BrowserCandidate {
        name: "Google Chrome",
        locate: Locate::OnPath("google-chrome"),
    },
    BrowserCandidate {
        name: "Google Chrome",
        locate: Locate::OnPath("google-chrome-stable"),
    },
    BrowserCandidate {
        name: "Chromium",
        locate: Locate::OnPath("chromium"),
    },
    BrowserCandidate {
        name: "Chromium",
        locate: Locate::OnPath("chromium-browser"),
    },
    BrowserCandidate {
        name: "Brave",
        locate: Locate::OnPath("brave-browser"),
    },
    BrowserCandidate {
        name: "Microsoft Edge",
        locate: Locate::OnPath("microsoft-edge"),
    },
    BrowserCandidate {
        name: "Microsoft Edge",
        locate: Locate::OnPath("microsoft-edge-stable"),
    },
    BrowserCandidate {
        name: "Vivaldi",
        locate: Locate::OnPath("vivaldi"),
    },
];

/// The candidates for the running operating system.
pub fn candidates() -> &'static [BrowserCandidate] {
    if cfg!(target_os = "macos") {
        MACOS_CANDIDATES
    } else if cfg!(target_os = "linux") {
        LINUX_CANDIDATES
    } else {
        &[]
    }
}

/// Where discovery looks. Tests point these at temporary directories.
#[derive(Debug, Clone)]
pub struct BrowserSearch {
    pub path_var: Option<std::ffi::OsString>,
    /// Application directories, each searched for bundle executables.
    pub applications: Vec<PathBuf>,
}

impl BrowserSearch {
    /// This machine: the process `PATH`, and on macOS `/Applications` plus
    /// `~/Applications`.
    pub fn host() -> Self {
        Self {
            path_var: std::env::var_os("PATH"),
            applications: host_applications(),
        }
    }
}

fn host_applications() -> Vec<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if home.is_absolute() {
            roots.push(home.join("Applications"));
        }
    }
    roots
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
/// name looked up on `PATH`), otherwise the first candidate in `search`.
pub fn discover(
    configured: Option<&str>,
    search: &BrowserSearch,
) -> Result<FoundBrowser, DiscoveryError> {
    discover_in(configured, search, candidates())
}

/// Every candidate found in `search`, one entry per distinct program path,
/// for the settings screen.
pub fn discover_all(search: &BrowserSearch) -> Vec<FoundBrowser> {
    discover_all_in(search, candidates())
}

fn discover_in(
    configured: Option<&str>,
    search: &BrowserSearch,
    candidates: &[BrowserCandidate],
) -> Result<FoundBrowser, DiscoveryError> {
    if let Some(configured) = configured.map(str::trim).filter(|value| !value.is_empty()) {
        let as_path = Path::new(configured);
        let found = if as_path.is_absolute() {
            executable_path(as_path)
        } else {
            find_on_path(configured, search.path_var.as_deref())
        };
        return found
            .map(|path| FoundBrowser {
                name: display_name(&path, candidates),
                path,
            })
            .ok_or_else(|| DiscoveryError::ConfiguredMissing(configured.to_string()));
    }
    discover_all_in(search, candidates)
        .into_iter()
        .next()
        .ok_or_else(|| DiscoveryError::NoneFound(install_hint(candidates)))
}

fn discover_all_in(search: &BrowserSearch, candidates: &[BrowserCandidate]) -> Vec<FoundBrowser> {
    let mut found: Vec<FoundBrowser> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        let Some(path) = locate(candidate, search) else {
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

/// The program launch can exec at `path`.
///
/// A regular executable is itself. A macOS `.app` bundle resolves to the
/// binary inside `Contents/MacOS`, which is the process that keeps the
/// DevTools pipes.
pub fn executable_path(path: &Path) -> Option<PathBuf> {
    if is_executable(path) {
        return Some(path.to_path_buf());
    }
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".app")?;
    let inner = path.join("Contents/MacOS").join(stem);
    is_executable(&inner).then(|| inner)
}

fn locate(candidate: &BrowserCandidate, search: &BrowserSearch) -> Option<PathBuf> {
    match candidate.locate {
        Locate::OnPath(program) => find_on_path(program, search.path_var.as_deref()),
        Locate::Application(relative) => search
            .applications
            .iter()
            .map(|root| root.join(relative))
            .find(|path| is_executable(path)),
    }
}

/// `Google Chrome, Chromium, Brave, …` without repeats.
fn install_hint(candidates: &[BrowserCandidate]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for candidate in candidates {
        if !names.contains(&candidate.name) {
            names.push(candidate.name);
        }
    }
    names.join(", ")
}

/// The display name of a browser program: the candidate's name when the
/// file is a known program, otherwise its file name.
fn display_name(path: &Path, candidates: &[BrowserCandidate]) -> String {
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    candidates
        .iter()
        .find(|candidate| program_file(candidate) == file)
        .map(|candidate| candidate.name.to_string())
        .unwrap_or(file)
}

fn program_file(candidate: &BrowserCandidate) -> &str {
    match candidate.locate {
        Locate::OnPath(name) => name,
        Locate::Application(relative) => relative.rsplit('/').next().unwrap_or(relative),
    }
}

fn find_on_path(program: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path_var?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
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
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use tempfile::tempdir;

    #[cfg(unix)]
    fn fake_program(dir: &Path, name: &str, executable: bool) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[cfg(unix)]
    fn search_path(path: &std::ffi::OsStr) -> BrowserSearch {
        BrowserSearch {
            path_var: Some(path.to_os_string()),
            applications: Vec::new(),
        }
    }

    #[test]
    fn linux_prefers_chrome_then_chromium_then_the_rest() {
        let programs: Vec<&str> = LINUX_CANDIDATES
            .iter()
            .map(|candidate| program_file(candidate))
            .collect();
        assert_eq!(programs[0], "google-chrome");
        assert!(programs.contains(&"brave-browser"));
        assert!(programs.contains(&"microsoft-edge"));
        assert!(programs.contains(&"vivaldi"));
        if cfg!(target_os = "linux") {
            assert_eq!(candidates(), LINUX_CANDIDATES);
        }
    }

    #[test]
    fn macos_looks_in_bundles_before_path() {
        assert!(matches!(
            MACOS_CANDIDATES[0].locate,
            Locate::Application(path) if path.ends_with("Contents/MacOS/Google Chrome")
        ));
        let first_path = MACOS_CANDIDATES
            .iter()
            .position(|candidate| matches!(candidate.locate, Locate::OnPath(_)))
            .unwrap();
        assert!(MACOS_CANDIDATES[..first_path]
            .iter()
            .all(|candidate| matches!(candidate.locate, Locate::Application(_))));
        if cfg!(target_os = "macos") {
            assert_eq!(candidates(), MACOS_CANDIDATES);
        }
    }

    #[test]
    #[cfg(unix)]
    fn the_first_candidate_on_path_wins() {
        let a = tempdir().unwrap();
        let b = tempdir().unwrap();
        fake_program(a.path(), "brave-browser", true);
        let chromium = fake_program(b.path(), "chromium", true);
        let path = std::env::join_paths([a.path(), b.path()]).unwrap();

        let found = discover_in(None, &search_path(&path), LINUX_CANDIDATES).unwrap();
        assert_eq!(found.path, chromium, "chromium ranks above brave");
        assert_eq!(found.name, "Chromium");
    }

    #[test]
    #[cfg(unix)]
    fn a_bundle_beats_a_path_shim() {
        let apps = tempdir().unwrap();
        let bin = tempdir().unwrap();
        let bundle = fake_program(
            apps.path(),
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            true,
        );
        fake_program(bin.path(), "chromium", true);
        let path = std::env::join_paths([bin.path()]).unwrap();
        let search = BrowserSearch {
            path_var: Some(path),
            applications: vec![apps.path().to_path_buf()],
        };
        let found = discover_in(None, &search, MACOS_CANDIDATES).unwrap();
        assert_eq!(found.path, bundle);
        assert_eq!(found.name, "Google Chrome");
        assert!(found.path.ends_with("Contents/MacOS/Google Chrome"));
    }

    #[test]
    #[cfg(unix)]
    fn a_configured_app_bundle_resolves_to_its_executable() {
        let apps = tempdir().unwrap();
        let inner = fake_program(
            apps.path(),
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            true,
        );
        let bundle = apps.path().join("Google Chrome.app");
        let found = discover_in(
            Some(bundle.to_str().unwrap()),
            &BrowserSearch::host(),
            MACOS_CANDIDATES,
        )
        .unwrap();
        assert_eq!(found.path, inner);
        assert_eq!(found.name, "Google Chrome");
    }

    #[test]
    #[cfg(unix)]
    fn a_file_that_is_not_executable_is_skipped() {
        let dir = tempdir().unwrap();
        fake_program(dir.path(), "google-chrome", false);
        let brave = fake_program(dir.path(), "brave-browser", true);
        let path = std::env::join_paths([dir.path()]).unwrap();
        assert_eq!(
            discover_in(None, &search_path(&path), LINUX_CANDIDATES)
                .unwrap()
                .path,
            brave
        );
    }

    #[test]
    #[cfg(unix)]
    fn the_configured_program_wins_as_a_path_or_a_name() {
        let dir = tempdir().unwrap();
        fake_program(dir.path(), "google-chrome", true);
        let custom = fake_program(dir.path(), "my-browser", true);
        let path = std::env::join_paths([dir.path()]).unwrap();
        let search = search_path(&path);

        let by_path =
            discover_in(Some(custom.to_str().unwrap()), &search, LINUX_CANDIDATES).unwrap();
        assert_eq!(by_path.path, custom);
        assert_eq!(by_path.name, "my-browser");

        let by_name = discover_in(Some("my-browser"), &search, LINUX_CANDIDATES).unwrap();
        assert_eq!(by_name.path, custom);

        let known = discover_in(Some("google-chrome"), &search, LINUX_CANDIDATES).unwrap();
        assert_eq!(known.name, "Google Chrome");
    }

    /// A configured browser that is missing is an error, not a silent
    /// fallback to some other browser.
    #[test]
    #[cfg(unix)]
    fn a_missing_configured_program_is_an_error() {
        let dir = tempdir().unwrap();
        fake_program(dir.path(), "google-chrome", true);
        let path = std::env::join_paths([dir.path()]).unwrap();
        let search = search_path(&path);
        assert_eq!(
            discover_in(Some("/nowhere/chrome"), &search, LINUX_CANDIDATES),
            Err(DiscoveryError::ConfiguredMissing("/nowhere/chrome".into()))
        );
        assert!(
            discover_in(Some("  "), &search, LINUX_CANDIDATES).is_ok(),
            "blank means unset"
        );
    }

    #[test]
    #[cfg(unix)]
    fn nothing_found_names_what_to_install() {
        let dir = tempdir().unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();
        let error = discover_in(None, &search_path(&path), LINUX_CANDIDATES).unwrap_err();
        let text = error.to_string();
        assert!(
            text.contains("Google Chrome") && text.contains("Brave"),
            "{text}"
        );
        let empty = BrowserSearch {
            path_var: None,
            applications: Vec::new(),
        };
        assert!(discover_in(None, &empty, LINUX_CANDIDATES).is_err());
    }

    #[test]
    #[cfg(unix)]
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
        let names: Vec<String> = discover_all_in(&search_path(&path), LINUX_CANDIDATES)
            .into_iter()
            .map(|browser| browser.name)
            .collect();
        assert_eq!(
            names,
            ["Chromium", "Brave"],
            "a symlink to the same program is listed once"
        );
    }
}
