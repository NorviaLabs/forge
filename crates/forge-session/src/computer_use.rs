//! Browser control and desktop control, for every session this process
//! opens.
//!
//! Each opened session gets the `browser_*` and `desktop_*` tools, backed
//! by a browser manager and a desktop of its own (children share them).
//! The tools are always registered; whether they are offered follows the
//! live settings, so turning a feature on or off in Settings reaches every
//! open session from its next request. This registry is how the desktop
//! app applies settings, shows the "Browser open" and "Controlling your
//! screen" strips, and answers Close browser and Stop.

use std::sync::{Arc, Mutex, OnceLock, Weak};

use forge_browser::{
    BrowserManager, BrowserStatus, ManagerOptions, WebCheckManager, WebCheckOptions,
};
use forge_config::{ComputerUseSettings, Config};
use forge_screen::{Desktop, ScreenController};
use forge_tools::ToolRegistry;
use forge_types::SessionId;
use serde::Serialize;

/// What the desktop app shows for one session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ComputerUseStatus {
    /// The session's browser, when one is running.
    pub browser: Option<BrowserStatus>,
    /// The session holds the screen.
    pub controlling_screen: bool,
    /// The web app `app_start` runs for the session, when it answers.
    pub app: Option<AppStatus>,
}

/// A running web app, as the palette and Activity offer it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppStatus {
    pub command: String,
    /// Where this machine reaches it: `http://127.0.0.1:<port>/`.
    pub url: String,
}

/// Every open session's browser manager and desktop.
pub struct ComputerUse {
    sessions: Mutex<Vec<Entry>>,
    /// Each opened session's app slot (weakly: closing the session drops it).
    apps: Mutex<Vec<(SessionId, Weak<forge_tools::web_app::WebAppSlot>)>>,
}

/// An opened session and its (weakly held) managers: closing the session
/// drops them.
type Entry = (
    SessionId,
    Weak<BrowserManager>,
    Weak<Desktop>,
    Weak<WebCheckManager>,
);

impl ComputerUse {
    /// The process-wide registry.
    pub fn global() -> &'static ComputerUse {
        static GLOBAL: OnceLock<ComputerUse> = OnceLock::new();
        GLOBAL.get_or_init(|| ComputerUse {
            sessions: Mutex::new(Vec::new()),
            apps: Mutex::new(Vec::new()),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Entry>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Register the computer-use tools for a session being opened with
    /// `cfg`. The returned handles are recorded with [`Self::register`]
    /// once the session has an id.
    pub fn install(&self, tools: &mut ToolRegistry, cfg: &Config) -> Installed {
        let browser = BrowserManager::new(cfg.browser.clone(), ManagerOptions::default());
        let desktop = Desktop::new(
            cfg.desktop.clone(),
            ScreenController::global(),
            Box::new(forge_screen::connect),
            std::process::id(),
        );
        for tool in forge_browser::browser_tools(browser.clone()) {
            tools.register(tool);
        }
        for tool in forge_screen::desktop_tools(desktop.clone()) {
            tools.register(tool);
        }
        // Web check: on by default, offered where the sandbox can relay an
        // app out; it shares the browser program with browser control.
        let web = WebCheckManager::new(
            cfg.web_check.clone(),
            cfg.browser.clone(),
            cfg.web.clone(),
            WebCheckOptions::default(),
        );
        for tool in forge_browser::web_tools(web.clone()) {
            tools.register(tool);
        }
        Installed {
            browser,
            desktop,
            web,
        }
    }

    /// Remember `installed` as the opened session `session`'s (its
    /// children's browsers live in the same manager).
    pub fn register(
        &self,
        session: SessionId,
        installed: Installed,
        web_app: &Arc<forge_tools::web_app::WebAppSlot>,
    ) {
        {
            let mut apps = self
                .apps
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            apps.retain(|(_, slot)| slot.strong_count() > 0);
            apps.push((session, Arc::downgrade(web_app)));
        }
        let mut sessions = self.lock();
        // Sessions that closed took their managers with them.
        sessions.retain(|(_, browser, desktop, web)| {
            browser.strong_count() > 0 && desktop.strong_count() > 0 && web.strong_count() > 0
        });
        sessions.push((
            session,
            Arc::downgrade(&installed.browser),
            Arc::downgrade(&installed.desktop),
            Arc::downgrade(&installed.web),
        ));
    }

    /// The handles registered for the opened session `session`.
    pub fn handles(&self, session: SessionId) -> Option<Installed> {
        self.lock()
            .iter()
            .rev()
            .find(|(id, _, _, _)| *id == session)
            .and_then(|(_, browser, desktop, web)| {
                Some(Installed {
                    browser: browser.upgrade()?,
                    desktop: desktop.upgrade()?,
                    web: web.upgrade()?,
                })
            })
    }

    fn live(&self) -> Vec<Installed> {
        self.lock()
            .iter()
            .filter_map(|(_, browser, desktop, web)| {
                Some(Installed {
                    browser: browser.upgrade()?,
                    desktop: desktop.upgrade()?,
                    web: web.upgrade()?,
                })
            })
            .collect()
    }

    /// Apply saved settings to every open session.
    pub fn apply_settings(&self, settings: &ComputerUseSettings) {
        for installed in self.live() {
            installed.browser.set_config(settings.browser.clone());
            installed.desktop.set_config(settings.desktop.clone());
            installed
                .web
                .set_config(settings.web_check.clone(), settings.browser.clone());
        }
    }

    /// What the strips show for `session` (a child's browser is found in
    /// its parent's manager).
    pub fn status(&self, session: SessionId) -> ComputerUseStatus {
        let live = self.live();
        ComputerUseStatus {
            browser: live
                .iter()
                .find_map(|installed| installed.browser.status(session)),
            controlling_screen: live
                .first()
                .is_some_and(|installed| installed.desktop.is_controlling(session)),
            app: self.app(session),
        }
    }

    /// The session's running app, when one answers.
    fn app(&self, session: SessionId) -> Option<AppStatus> {
        let slot = self
            .apps
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .rev()
            .find(|(id, _)| *id == session)
            .and_then(|(_, slot)| slot.upgrade())?;
        let app = slot.get()?;
        Some(AppStatus {
            command: app.command,
            url: format!("http://{}/", app.origin),
        })
    }

    /// Close the session's browser. `false` when none was running.
    pub async fn close_browser(&self, session: SessionId) -> bool {
        for installed in self.live() {
            if installed.browser.status(session).is_some() {
                return installed.browser.close(session).await;
            }
        }
        false
    }

    /// End the session's control of the screen.
    pub fn stop_screen(&self, session: SessionId) -> bool {
        self.live()
            .into_iter()
            .any(|installed| installed.desktop.stop(session))
    }
}

/// The browser manager, desktop and web check of one opened session.
#[derive(Clone)]
pub struct Installed {
    pub browser: Arc<BrowserManager>,
    pub desktop: Arc<Desktop>,
    pub web: Arc<WebCheckManager>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{open_session, SessionTarget};
    use forge_config::{BrowserConfig, DesktopConfig};
    use forge_test_support::init_repo;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn offered(opened: &crate::OpenedSession) -> Vec<String> {
        opened
            .session
            .build_model_request()
            .tools
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    }

    async fn opened(browser: BrowserConfig) -> (TempDir, crate::OpenedSession) {
        let temp = TempDir::new().unwrap();
        init_repo(temp.path());
        let mut cfg = Config::default();
        cfg.model.mock = true;
        cfg.resolved_workspace = temp.path().to_path_buf();
        cfg.workspace_root = Some(temp.path().display().to_string());
        cfg.browser = browser;
        let opened = open_session(&cfg, SessionTarget::New).await.unwrap();
        (temp, opened)
    }

    fn fake_browser(dir: &std::path::Path) -> String {
        let path = dir.join("fake-chromium");
        std::fs::write(&path, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    /// These tests change process-wide settings; one at a time.
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn the_tools_are_registered_but_not_offered_while_off() {
        let _serial = SERIAL.lock().await;
        let (_temp, opened) = opened(BrowserConfig::default()).await;
        let names = offered(&opened);
        assert!(
            !names.iter().any(|name| name.starts_with("browser_")),
            "{names:?}"
        );
        assert!(
            !names.iter().any(|name| name.starts_with("desktop_")),
            "{names:?}"
        );
        let status = ComputerUse::global().status(opened.session.session_id);
        assert_eq!(status, ComputerUseStatus::default());
    }

    #[tokio::test]
    async fn a_user_config_with_browser_control_offers_the_tools() {
        let _serial = SERIAL.lock().await;
        let dir = TempDir::new().unwrap();
        let (_temp, opened) = opened(BrowserConfig {
            enabled: true,
            executable: Some(fake_browser(dir.path())),
            headless: true,
        })
        .await;
        let names = offered(&opened);
        for tool in forge_browser::BROWSER_TOOL_NAMES {
            // The screenshot tool needs a model that sees images; the
            // offline mock does not.
            if tool == "browser_screenshot" {
                assert!(!names.contains(&tool.to_string()), "{tool}");
            } else {
                assert!(names.contains(&tool.to_string()), "{tool}: {names:?}");
            }
        }
    }

    /// Saving settings reaches a session that is already open.
    #[tokio::test]
    async fn saved_settings_reach_open_sessions() {
        let _serial = SERIAL.lock().await;
        let (_temp, opened) = opened(BrowserConfig::default()).await;
        let dir = TempDir::new().unwrap();
        let mine = ComputerUse::global()
            .handles(opened.session.session_id)
            .expect("this session's handles are registered");

        mine.browser.set_config(BrowserConfig {
            enabled: true,
            executable: Some(fake_browser(dir.path())),
            headless: true,
        });
        assert!(offered(&opened).contains(&"browser_navigate".to_string()));

        ComputerUse::global().apply_settings(&ComputerUseSettings {
            browser: BrowserConfig::default(),
            desktop: DesktopConfig { enabled: true },
            web_check: Default::default(),
        });
        assert!(!offered(&opened).contains(&"browser_navigate".to_string()));
        assert!(
            mine.desktop.config().enabled,
            "the desktop setting was applied too"
        );
        assert!(
            !offered(&opened)
                .iter()
                .any(|name| name.starts_with("desktop_")),
            "desktop tools need a model that sees images"
        );
        ComputerUse::global().apply_settings(&ComputerUseSettings::default());
    }

    async fn opened_with(edit: impl FnOnce(&mut Config)) -> (TempDir, crate::OpenedSession) {
        let temp = TempDir::new().unwrap();
        init_repo(temp.path());
        let mut cfg = Config::default();
        cfg.model.mock = true;
        cfg.resolved_workspace = temp.path().to_path_buf();
        cfg.workspace_root = Some(temp.path().display().to_string());
        edit(&mut cfg);
        let opened = open_session(&cfg, SessionTarget::New).await.unwrap();
        (temp, opened)
    }

    /// Web check is on by default and needs no browser control: offered
    /// wherever the sandbox can relay an app and a browser is found, and
    /// gone once the user turns it off.
    #[tokio::test]
    async fn the_web_check_tools_follow_the_switch_and_the_machine() {
        let _serial = SERIAL.lock().await;
        let dir = TempDir::new().unwrap();
        let browser = BrowserConfig {
            enabled: false,
            executable: Some(fake_browser(dir.path())),
            headless: true,
        };
        let (_temp, opened) = opened_with(|cfg| cfg.browser = browser.clone()).await;
        let mine = ComputerUse::global()
            .handles(opened.session.session_id)
            .expect("registered");
        assert!(
            mine.web.browser_path().is_some(),
            "the browser control row's program"
        );
        let supported = cfg!(target_os = "linux") && forge_tools::sandbox::relay_available();
        for tool in forge_browser::WEB_TOOL_NAMES {
            assert_eq!(
                offered(&opened).contains(&tool.to_string()),
                supported,
                "{tool}"
            );
        }
        ComputerUse::global().apply_settings(&ComputerUseSettings {
            browser,
            desktop: DesktopConfig::default(),
            web_check: forge_config::WebCheckConfig { enabled: false },
        });
        assert!(!mine.web.is_available());
        for tool in forge_browser::WEB_TOOL_NAMES {
            assert!(!offered(&opened).contains(&tool.to_string()), "{tool}");
        }
        ComputerUse::global().apply_settings(&ComputerUseSettings::default());
    }

    /// The project's `[web]` section reaches the session: the tools'
    /// defaults and how long an app may take to answer.
    #[tokio::test]
    async fn the_projects_web_app_reaches_the_session() {
        let _serial = SERIAL.lock().await;
        let web = forge_config::WebConfig {
            start: Some("npm run dev".into()),
            port: Some(5173),
            path: None,
            ready_timeout_secs: Some(90),
        };
        let (_temp, opened) = opened_with(|cfg| cfg.web = web.clone()).await;
        let mine = ComputerUse::global()
            .handles(opened.session.session_id)
            .expect("registered");
        assert_eq!(mine.web.web(), &web);
        assert_eq!(
            opened.session.web_ready_timeout(),
            std::time::Duration::from_secs(90)
        );
        let (_temp, plain) = opened_with(|_| {}).await;
        assert_eq!(
            plain.session.web_ready_timeout(),
            std::time::Duration::from_secs(60)
        );
    }

    /// The app `app_start` records is in the session's status (for the
    /// Open link and the palette), and leaves it when it stops.
    #[tokio::test]
    async fn a_running_app_is_in_the_status() {
        let _serial = SERIAL.lock().await;
        let (_temp, opened) = opened_with(|_| {}).await;
        let session = opened.session.session_id;
        assert_eq!(ComputerUse::global().status(session).app, None);
        let slot = opened.session.web_app_slot();
        slot.set(forge_tools::web_app::RunningApp {
            generation: 1,
            command: "npm run dev".into(),
            port: 5173,
            origin: "127.0.0.1:43817".parse().unwrap(),
        });
        assert_eq!(
            ComputerUse::global().status(session).app,
            Some(AppStatus {
                command: "npm run dev".into(),
                url: "http://127.0.0.1:43817/".into(),
            })
        );
        slot.clear(1);
        assert_eq!(ComputerUse::global().status(session).app, None);
        // Another session sees nothing of it.
        slot.set(forge_tools::web_app::RunningApp {
            generation: 2,
            command: "npm run dev".into(),
            port: 5173,
            origin: "127.0.0.1:43817".parse().unwrap(),
        });
        assert_eq!(ComputerUse::global().status(SessionId::new_v4()).app, None);
    }

    #[tokio::test]
    async fn closing_and_stopping_an_unknown_session_do_nothing() {
        let unknown = SessionId::new_v4();
        assert!(!ComputerUse::global().close_browser(unknown).await);
        assert!(!ComputerUse::global().stop_screen(unknown));
    }
}
