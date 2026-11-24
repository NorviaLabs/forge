//! Browser control and desktop control for one opened session.
//!
//! Each session gets the `browser_*` and `desktop_*` tools, backed by a
//! browser manager and a desktop it owns (a child shares its parent's).
//! The tools are always registered; whether they are offered follows the
//! live settings, so turning a feature on or off in Settings reaches an
//! open session from its next request. The session holds these handles.
//! The supervisor reads them for the "Browser open" and "Controlling your
//! screen" strips, and for Close browser and Stop.

use std::sync::Arc;

use forge_browser::{
    BrowserLocator, BrowserManager, BrowserStatus, ManagerOptions, WebCheckManager, WebCheckOptions,
};
use forge_config::{ComputerUseSettings, Config};
use forge_core::AgentSession;
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

/// Register the computer-use tools for a session being opened with `cfg`.
///
/// Browser control and web check share one [`BrowserLocator`], so they
/// start the same program. The caller stores the handles on the session.
pub fn install(tools: &mut ToolRegistry, cfg: &Config) -> Installed {
    let locator = BrowserLocator::host();
    let browser = BrowserManager::new(
        cfg.browser.clone(),
        ManagerOptions {
            locator: locator.clone(),
            ..ManagerOptions::default()
        },
    );
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
    // app out. It uses the same locator as browser control.
    let web = WebCheckManager::new(
        cfg.web_check.clone(),
        cfg.browser.clone(),
        cfg.web.clone(),
        WebCheckOptions {
            locator,
            ..WebCheckOptions::default()
        },
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

/// The handles stored on a session, when it has them.
pub fn of_session(session: &AgentSession) -> Option<Arc<Installed>> {
    session
        .service()
        .and_then(|service| service.downcast::<Installed>().ok())
}

/// The browser manager, desktop and web check of one opened session.
#[derive(Clone)]
pub struct Installed {
    pub browser: Arc<BrowserManager>,
    pub desktop: Arc<Desktop>,
    pub web: Arc<WebCheckManager>,
}

impl Installed {
    /// What the strips show for this session.
    pub fn status(&self, session: SessionId, agent: &AgentSession) -> ComputerUseStatus {
        ComputerUseStatus {
            browser: self.browser.status(session),
            controlling_screen: self.desktop.is_controlling(session),
            app: app_status(&agent.web_app_slot()),
        }
    }

    /// Apply saved settings to this session. A browser already running keeps
    /// running until it is closed.
    pub fn apply_settings(&self, settings: &ComputerUseSettings) {
        self.browser.set_config(settings.browser.clone());
        self.desktop.set_config(settings.desktop.clone());
        self.web
            .set_config(settings.web_check.clone(), settings.browser.clone());
    }

    /// Close this session's browser. `false` when none was running.
    pub async fn close_browser(&self, session: SessionId) -> bool {
        if self.browser.status(session).is_none() {
            return false;
        }
        self.browser.close(session).await
    }

    /// End this session's control of the screen.
    pub fn stop_screen(&self, session: SessionId) -> bool {
        self.desktop.stop(session)
    }
}

/// The session's running app, when one answers.
pub fn app_status(slot: &forge_tools::web_app::WebAppSlot) -> Option<AppStatus> {
    let app = slot.get()?;
    Some(AppStatus {
        command: app.command,
        url: format!("http://{}/", app.origin),
    })
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

    fn handles(opened: &crate::OpenedSession) -> Arc<super::Installed> {
        super::of_session(&opened.session).expect("the session owns its computer-use handles")
    }

    #[tokio::test]
    async fn the_tools_are_registered_but_not_offered_while_off() {
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
        let status = handles(&opened).status(opened.session.session_id, &opened.session);
        assert_eq!(status, ComputerUseStatus::default());
    }

    #[tokio::test]
    async fn a_user_config_with_browser_control_offers_the_tools() {
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
        let (_temp, opened) = opened(BrowserConfig::default()).await;
        let dir = TempDir::new().unwrap();
        let mine = handles(&opened);

        mine.browser.set_config(BrowserConfig {
            enabled: true,
            executable: Some(fake_browser(dir.path())),
            headless: true,
        });
        assert!(offered(&opened).contains(&"browser_navigate".to_string()));

        mine.apply_settings(&ComputerUseSettings {
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
        let dir = TempDir::new().unwrap();
        let browser = BrowserConfig {
            enabled: false,
            executable: Some(fake_browser(dir.path())),
            headless: true,
        };
        let (_temp, opened) = opened_with(|cfg| cfg.browser = browser.clone()).await;
        let mine = handles(&opened);
        let program = mine
            .web
            .browser_path()
            .expect("the browser control row's program");
        assert_eq!(
            mine.browser.found().unwrap().path,
            program,
            "browser control and web check share one lookup"
        );
        let supported = cfg!(target_os = "linux") && forge_tools::sandbox::relay_available();
        for tool in forge_browser::WEB_TOOL_NAMES {
            assert_eq!(
                offered(&opened).contains(&tool.to_string()),
                supported,
                "{tool}"
            );
        }
        mine.apply_settings(&ComputerUseSettings {
            browser,
            desktop: DesktopConfig::default(),
            web_check: forge_config::WebCheckConfig { enabled: false },
        });
        assert!(!mine.web.is_available());
        for tool in forge_browser::WEB_TOOL_NAMES {
            assert!(!offered(&opened).contains(&tool.to_string()), "{tool}");
        }
    }

    /// The project's `[web]` section reaches the session: the tools'
    /// defaults and how long an app may take to answer.
    #[tokio::test]
    async fn the_projects_web_app_reaches_the_session() {
        let web = forge_config::WebConfig {
            start: Some("npm run dev".into()),
            port: Some(5173),
            path: None,
            ready_timeout_secs: Some(90),
        };
        let (_temp, opened) = opened_with(|cfg| cfg.web = web.clone()).await;
        let mine = handles(&opened);
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
        let (_temp, opened) = opened_with(|_| {}).await;
        let mine = handles(&opened);
        let session = opened.session.session_id;
        assert_eq!(mine.status(session, &opened.session).app, None);
        let slot = opened.session.web_app_slot();
        slot.set(forge_tools::web_app::RunningApp {
            generation: 1,
            command: "npm run dev".into(),
            port: 5173,
            origin: "127.0.0.1:43817".parse().unwrap(),
        });
        assert_eq!(
            mine.status(session, &opened.session).app,
            Some(AppStatus {
                command: "npm run dev".into(),
                url: "http://127.0.0.1:43817/".into(),
            })
        );
        slot.clear(1);
        assert_eq!(mine.status(session, &opened.session).app, None);
        // Another session does not see this one's app.
        slot.set(forge_tools::web_app::RunningApp {
            generation: 2,
            command: "npm run dev".into(),
            port: 5173,
            origin: "127.0.0.1:43817".parse().unwrap(),
        });
        let (_temp, other) = opened_with(|_| {}).await;
        assert_eq!(
            handles(&other)
                .status(other.session.session_id, &other.session)
                .app,
            None
        );
    }

    #[tokio::test]
    async fn closing_and_stopping_an_unknown_session_do_nothing() {
        let (_temp, opened) = opened_with(|_| {}).await;
        let mine = handles(&opened);
        let unknown = SessionId::new_v4();
        assert!(!mine.close_browser(unknown).await);
        assert!(!mine.stop_screen(unknown));
    }
}
