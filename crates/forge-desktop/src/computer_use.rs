//! Settings → Computer use: what the page shows, and saving it.
//!
//! The settings live in the user's own `config.toml` (a project cannot turn
//! them on). Saving writes the two sections and applies them to every open
//! session at once.

use std::path::Path;

use forge_browser::FoundBrowser;
use forge_config::ComputerUseSettings;
use forge_screen::Unsupported;
use serde::Serialize;

/// The Computer use section of Settings.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ComputerUseSettingsView {
    pub settings: ComputerUseSettings,
    /// Browsers found on this machine, for the picker.
    pub browsers: Vec<FoundBrowser>,
    /// Shown under "Browser" when nothing can be started.
    pub browser_problem: Option<String>,
    /// Why desktop control cannot run here; the toggle is disabled with it.
    pub desktop_unsupported: Option<String>,
    /// Why web check cannot run here (besides a missing browser); the
    /// toggle is disabled with it.
    pub web_check_unsupported: Option<String>,
}

/// Whether web check can run on this machine: Linux, a working sandbox,
/// and socat to relay the app out of it.
pub fn web_check_support(
    linux: bool,
    sandbox: Result<(), String>,
    relay: bool,
) -> Result<(), String> {
    if !linux {
        return Err("Web check runs on Linux for now.".into());
    }
    if let Err(reason) = sandbox {
        return Err(format!(
            "Needs the sandbox, which is not available here: {reason}"
        ));
    }
    if !relay {
        return Err(
            "Needs socat, which relays the app out of the sandbox. Install it (for example \
             `apt install socat`)."
                .into(),
        );
    }
    Ok(())
}

/// The URL of the session's running app, to open in the user's browser.
/// Built from what the session recorded, never from the page.
pub fn app_url(status: &forge_session::computer_use::ComputerUseStatus) -> Result<String, String> {
    status
        .app
        .as_ref()
        .map(|app| app.url.clone())
        .ok_or_else(|| "no app is running in this session".to_string())
}

/// Build the section from the saved settings and what this machine has.
pub fn settings_view(
    settings: ComputerUseSettings,
    browsers: Vec<FoundBrowser>,
    desktop: Result<(), Unsupported>,
    web_check: Result<(), String>,
) -> ComputerUseSettingsView {
    let chosen = settings
        .browser
        .executable
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty());
    let browser_problem = match chosen {
        Some(path) if forge_browser::executable_path(Path::new(path)).is_none() => Some(format!(
            "The chosen browser {path} is missing or cannot be run."
        )),
        None if browsers.is_empty() => {
            Some("No Chrome, Chromium, Brave, Edge or Vivaldi found.".to_string())
        }
        _ => None,
    };
    ComputerUseSettingsView {
        settings,
        browsers,
        browser_problem,
        desktop_unsupported: desktop.err().map(|reason| reason.to_string()),
        web_check_unsupported: web_check.err(),
    }
}

/// Check settings from the page before they are saved: a chosen browser
/// must be an absolute path to an executable file; blank means detect.
pub fn checked_settings(
    mut input: ComputerUseSettings,
    project_roots: &[std::path::PathBuf],
) -> Result<ComputerUseSettings, String> {
    let executable = input
        .browser
        .executable
        .take()
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty());
    let stored = if let Some(path) = &executable {
        let as_path = Path::new(path);
        if !as_path.is_absolute() {
            return Err(format!("choose the browser by its full path, not `{path}`"));
        }
        // A macOS .app bundle is stored as the executable inside it: that is
        // the process launch attaches the DevTools pipes to.
        let resolved = forge_browser::executable_path(as_path)
            .ok_or_else(|| format!("{path} is not a program that can be run"))?;
        // Runs outside the sandbox: not a program the agent could have
        // written (anything in an open project), nor one others can change.
        let real = std::fs::canonicalize(&resolved).map_err(|error| error.to_string())?;
        for root in project_roots {
            let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
            if real.starts_with(&root) {
                return Err(format!(
                    "{path} is inside the project {}, where the agent can write; choose an \
                     installed browser",
                    root.display()
                ));
            }
        }
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&real)
                .map_err(|error| error.to_string())?
                .permissions()
                .mode();
            if mode & 0o022 != 0 {
                return Err(format!(
                    "{path} is writable by other users; choose a program only you or the \
                     system can change"
                ));
            }
        }
        Some(resolved.display().to_string())
    } else {
        None
    };
    input.browser.executable = stored;
    Ok(input)
}

/// Check the settings and write them to `config_path`. The caller applies
/// them to open sessions.
pub fn save(
    config_path: &Path,
    input: ComputerUseSettings,
    project_roots: &[std::path::PathBuf],
) -> Result<ComputerUseSettings, String> {
    let settings = checked_settings(input, project_roots)?;
    forge_config::write_computer_use_settings(config_path, &settings)
        .map_err(|error| format!("could not save the settings: {error}"))?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_config::{BrowserConfig, DesktopConfig};
    use std::os::unix::fs::PermissionsExt;

    fn chrome() -> FoundBrowser {
        FoundBrowser {
            name: "Google Chrome".into(),
            path: "/usr/bin/google-chrome".into(),
        }
    }

    /// Web check needs Linux, the sandbox and socat; each missing piece is
    /// named, the most basic first.
    #[test]
    fn web_check_support_names_what_is_missing() {
        assert_eq!(web_check_support(true, Ok(()), true), Ok(()));
        let not_linux = web_check_support(false, Ok(()), true).unwrap_err();
        assert!(not_linux.contains("Linux"), "{not_linux}");
        let no_sandbox =
            web_check_support(true, Err("bubblewrap is not installed".into()), true).unwrap_err();
        assert!(
            no_sandbox.contains("bubblewrap is not installed"),
            "{no_sandbox}"
        );
        let no_relay = web_check_support(true, Ok(()), false).unwrap_err();
        assert!(no_relay.contains("socat"), "{no_relay}");
    }

    #[test]
    fn the_web_check_reason_reaches_the_page() {
        let view = settings_view(
            ComputerUseSettings::default(),
            vec![chrome()],
            Ok(()),
            Err("Needs socat.".into()),
        );
        assert_eq!(view.web_check_unsupported.as_deref(), Some("Needs socat."));
        assert!(view.settings.web_check.enabled, "on by default");
    }

    /// The Open link opens the session's own app only; with none there is
    /// nothing to open.
    #[test]
    fn only_the_sessions_own_app_is_opened() {
        use forge_session::computer_use::{AppStatus, ComputerUseStatus};
        let none = app_url(&ComputerUseStatus::default()).unwrap_err();
        assert!(none.contains("no app is running"), "{none}");
        let status = ComputerUseStatus {
            app: Some(AppStatus {
                command: "npm run dev".into(),
                url: "http://127.0.0.1:43817/".into(),
            }),
            ..ComputerUseStatus::default()
        };
        assert_eq!(app_url(&status).as_deref(), Ok("http://127.0.0.1:43817/"));
    }

    #[test]
    fn an_available_machine_shows_its_browsers_and_no_problems() {
        let view = settings_view(
            ComputerUseSettings::default(),
            vec![chrome()],
            Ok(()),
            Ok(()),
        );
        assert_eq!(view.browsers, vec![chrome()]);
        assert_eq!(view.browser_problem, None);
        assert_eq!(view.desktop_unsupported, None);
    }

    #[test]
    fn nothing_found_and_wayland_are_explained() {
        let view = settings_view(
            ComputerUseSettings::default(),
            vec![],
            Err(Unsupported::Wayland),
            Ok(()),
        );
        assert_eq!(
            view.browser_problem.as_deref(),
            Some("No Chrome, Chromium, Brave, Edge or Vivaldi found.")
        );
        assert!(view
            .desktop_unsupported
            .as_deref()
            .unwrap()
            .starts_with("Needs an X11 session."));
    }

    /// A chosen program that is gone is a problem even when others exist.
    #[test]
    fn a_missing_chosen_browser_is_a_problem() {
        let settings = ComputerUseSettings {
            browser: BrowserConfig {
                enabled: true,
                executable: Some("/nowhere/brave".into()),
                headless: false,
            },
            desktop: DesktopConfig::default(),
            web_check: Default::default(),
        };
        let view = settings_view(settings, vec![chrome()], Ok(()), Ok(()));
        assert!(view.browser_problem.unwrap().contains("/nowhere/brave"));
    }

    #[test]
    fn a_chosen_browser_must_be_an_absolute_executable() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("brave");
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let with = |executable: Option<&str>| ComputerUseSettings {
            browser: BrowserConfig {
                enabled: true,
                executable: executable.map(str::to_string),
                headless: false,
            },
            desktop: DesktopConfig::default(),
            web_check: Default::default(),
        };

        let ok = checked_settings(with(Some(program.to_str().unwrap())), &[]).unwrap();
        assert_eq!(ok.browser.executable.as_deref(), program.to_str());
        let bundle = dir.path().join("Chromium.app");
        let inner = bundle.join("Contents/MacOS/Chromium");
        std::fs::create_dir_all(inner.parent().unwrap()).unwrap();
        std::fs::write(&inner, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o755)).unwrap();
        let from_bundle = checked_settings(with(Some(bundle.to_str().unwrap())), &[]).unwrap();
        assert_eq!(
            from_bundle.browser.executable.as_deref(),
            inner.to_str(),
            "a bundle is stored as the executable inside it"
        );
        assert_eq!(
            checked_settings(with(Some("  ")), &[])
                .unwrap()
                .browser
                .executable,
            None,
            "blank means detect"
        );
        assert!(
            checked_settings(with(Some("brave")), &[]).is_err(),
            "relative"
        );
        assert!(
            checked_settings(with(Some("/nowhere/brave")), &[]).is_err(),
            "missing"
        );
        let text = dir.path().join("notes.txt");
        std::fs::write(&text, "x").unwrap();
        assert!(
            checked_settings(with(Some(text.to_str().unwrap())), &[]).is_err(),
            "not executable"
        );
    }

    /// The agent can write files in a workspace: a program there is not
    /// started as the browser, nor is one anybody else can change.
    #[test]
    fn a_browser_the_agent_or_others_could_have_written_is_refused() {
        let workspace = tempfile::tempdir().unwrap();
        let inside = workspace.path().join("tools").join("chrome");
        std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
        std::fs::write(&inside, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&inside, std::fs::Permissions::from_mode(0o755)).unwrap();
        let with = |path: &std::path::Path| ComputerUseSettings {
            browser: BrowserConfig {
                enabled: true,
                executable: Some(path.display().to_string()),
                headless: false,
            },
            desktop: DesktopConfig::default(),
            web_check: Default::default(),
        };
        let roots = [workspace.path().to_path_buf()];
        let error = checked_settings(with(&inside), &roots).unwrap_err();
        assert!(error.contains("project"), "{error}");
        // Through a link from outside, too.
        let elsewhere = tempfile::tempdir().unwrap();
        let link = elsewhere.path().join("chrome");
        std::os::unix::fs::symlink(&inside, &link).unwrap();
        assert!(checked_settings(with(&link), &roots).is_err());

        let shared = elsewhere.path().join("shared-chrome");
        std::fs::write(&shared, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        let error = checked_settings(with(&shared), &[]).unwrap_err();
        assert!(error.contains("writable"), "{error}");
    }

    #[test]
    fn saving_writes_the_users_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("forge").join("config.toml");
        let settings = ComputerUseSettings {
            browser: BrowserConfig {
                enabled: true,
                executable: None,
                headless: true,
            },
            desktop: DesktopConfig { enabled: false },
            web_check: Default::default(),
        };
        assert_eq!(save(&path, settings.clone(), &[]).unwrap(), settings);
        assert_eq!(
            forge_config::read_computer_use_settings(&path).unwrap(),
            settings
        );
        assert!(save(
            &path,
            ComputerUseSettings {
                browser: BrowserConfig {
                    enabled: true,
                    executable: Some("relative".into()),
                    headless: false,
                },
                desktop: DesktopConfig::default(),
                web_check: Default::default(),
            },
            &[]
        )
        .is_err());
        assert_eq!(
            forge_config::read_computer_use_settings(&path).unwrap(),
            settings,
            "a refused save leaves the file alone"
        );
    }
}
