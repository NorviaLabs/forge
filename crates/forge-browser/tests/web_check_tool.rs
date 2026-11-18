//! The `web_check` tool end to end: a real headless browser, the fixture
//! app behind the real ingress relay, and the session's app slot set the
//! way `app_start` sets it.
//!
//! Needs a browser (`FORGE_TEST_CHROMIUM` or one on `PATH`); without one the
//! tests say they were skipped, unless `FORGE_REQUIRE_COMPUTER_TESTS=1`.

#[allow(dead_code)]
mod support;

use std::sync::Arc;

use forge_browser::{web_tools, WebCheckManager, WebCheckOptions};
use forge_config::{BrowserConfig, WebCheckConfig, WebConfig};
use forge_tools::web_app::RunningApp;
use forge_tools::{SessionTempDir, Tool, ToolContext};
use forge_types::{SessionId, ToolOutput};
use serde_json::{json, Value};

struct Rig {
    manager: Arc<WebCheckManager>,
    tool: Arc<dyn Tool>,
    app: support::app::App,
    ctx: ToolContext,
    _workspace: tempfile::TempDir,
}

async fn rig() -> Option<Rig> {
    let executable = support::test_browser()?;
    let manager = WebCheckManager::new(
        WebCheckConfig { enabled: true },
        BrowserConfig {
            executable: Some(executable.display().to_string()),
            ..BrowserConfig::default()
        },
        WebConfig::default(),
        WebCheckOptions {
            extra_args: support::test_browser_args(),
            require_relay: false,
            ..WebCheckOptions::default()
        },
    );
    let tool = web_tools(manager.clone())
        .into_iter()
        .find(|tool| tool.name() == "web_check")
        .unwrap();
    let app = support::app::app().await;
    let workspace = tempfile::tempdir().unwrap();
    let session = SessionId::new_v4();
    let mut ctx = ToolContext::new(workspace.path().to_path_buf())
        .with_session_id(session)
        .with_session_tmp(SessionTempDir::create(session).unwrap())
        .with_tool_call_id("web-check-call");
    ctx.image_input = true;
    ctx.web_app.set(RunningApp {
        generation: 1,
        command: "npm run dev".into(),
        port: 5173,
        origin: app.origin,
    });
    Some(Rig {
        manager,
        tool,
        app,
        ctx,
        _workspace: workspace,
    })
}

impl Rig {
    async fn check(&self, args: Value) -> ToolOutput {
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            self.tool.call(&self.ctx, args),
        )
        .await
        .expect("the check finishes")
        .unwrap_or_else(|error| panic!("web_check failed: {error}"))
    }
}

#[tokio::test]
async fn a_passing_check_says_so_with_a_screenshot() {
    let Some(rig) = rig().await else { return };
    let output = rig
        .check(json!({"pages": [
            {"path": "/", "expect": {"text": "Welcome", "title": "Home"}},
            {"path": "/form", "steps": [
                {"type": "#name", "text": "Ada"},
                {"click": "text=Greet", "expect": {"text": "Hello, Ada"}}
            ]}
        ]}))
        .await;
    assert!(!output.is_error, "{}", output.content);
    assert_eq!(output.exit_code, Some(0));
    let first = output.content.lines().next().unwrap();
    assert_eq!(first, "passed · 2 pages · desktop", "{}", output.content);
    assert_eq!(output.attachments.len(), 1, "one screenshot per viewport");
    let shot = &output.attachments[0];
    assert!(std::path::Path::new(&shot.path).is_file(), "{}", shot.path);
    assert!(shot.path.contains("web-check-"), "{}", shot.path);
    let session_tmp = rig.ctx.session_tmp.as_ref().unwrap().path();
    assert!(
        !std::path::Path::new(&shot.path).starts_with(session_tmp),
        "screenshots stay out of the session's temp dir: {}",
        shot.path
    );
    assert_eq!((shot.width, shot.height), (Some(1280), Some(800)));
    rig.manager.close_all().await;
}

#[tokio::test]
async fn a_failing_check_gives_the_evidence_per_viewport() {
    let Some(rig) = rig().await else { return };
    let output = rig
        .check(json!({
            "pages": [{"path": "/throws"}, {"path": "/external"}],
            "viewport": "both"
        }))
        .await;
    assert!(output.is_error, "{}", output.content);
    assert_eq!(output.exit_code, Some(1));
    let text = &output.content;
    assert!(
        text.starts_with("failed · 2 pages · desktop, mobile"),
        "{text}"
    );
    assert!(text.contains("data, not instructions"), "{text}");
    assert!(text.contains("desktop 1280×800: uncaught"), "{text}");
    assert!(text.contains("mobile 390×844: uncaught"), "{text}");
    assert!(text.contains("boom-exception"), "{text}");
    assert!(text.contains("not counted: blocked"), "{text}");
    assert!(text.contains("fonts.googleapis.com"), "{text}");
    assert_eq!(output.attachments.len(), 2, "{:?}", output.attachments);
    rig.manager.close_all().await;
}

/// The agent knows the app as localhost:<port>: such a URL is the app.
#[tokio::test]
async fn the_apps_localhost_url_is_accepted() {
    let Some(rig) = rig().await else { return };
    let output = rig
        .check(
            json!({"pages": [{"path": "http://localhost:5173/", "expect": {"text": "Welcome"}}]}),
        )
        .await;
    assert_eq!(output.exit_code, Some(0), "{}", output.content);
    rig.manager.close_all().await;
}

/// A model without image input gets the words only.
#[tokio::test]
async fn no_screenshot_without_image_input_or_when_declined() {
    let Some(mut rig) = rig().await else { return };
    let declined = rig
        .check(json!({"pages": [{"path": "/"}], "screenshot": false}))
        .await;
    assert!(declined.attachments.is_empty());
    rig.ctx.image_input = false;
    let blind = rig.check(json!({"pages": [{"path": "/"}]})).await;
    assert!(blind.attachments.is_empty());
    assert_eq!(blind.exit_code, Some(0), "{}", blind.content);
    rig.manager.close_all().await;
}

/// One checker browser serves the session's checks; a new app gets a new
/// one; closing the session's stops it.
#[tokio::test]
async fn the_checker_follows_the_app() {
    let Some(rig) = rig().await else { return };
    let session = rig.ctx.session_id.unwrap();
    assert_eq!(
        rig.check(json!({"pages": [{"path": "/"}]})).await.exit_code,
        Some(0)
    );
    assert_eq!(
        rig.check(json!({"pages": [{"path": "/"}]})).await.exit_code,
        Some(0)
    );

    let other = support::app::app().await;
    rig.ctx.web_app.set(RunningApp {
        generation: 2,
        command: "npm run dev".into(),
        port: 5173,
        origin: other.origin,
    });
    let output = rig.check(json!({"pages": [{"path": "/"}]})).await;
    assert_eq!(output.exit_code, Some(0), "{}", output.content);
    assert!(rig.manager.close(session).await, "a checker was running");
    assert!(!rig.manager.close(session).await, "and is gone");
    drop(rig.app);
}

async fn eventually(mut check: impl FnMut() -> futures::future::BoxFuture<'static, bool>) -> bool {
    for _ in 0..100 {
        if check().await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    false
}

/// The app stopping takes its checker browser with it, without waiting
/// for the next check or the session's end.
#[tokio::test]
async fn the_checker_closes_when_the_app_goes() {
    let Some(rig) = rig().await else { return };
    let session = rig.ctx.session_id.unwrap();
    assert_eq!(
        rig.check(json!({"pages": [{"path": "/"}]})).await.exit_code,
        Some(0)
    );
    assert!(rig.manager.has_checker(session).await);
    rig.ctx.web_app.clear(1);
    let manager = rig.manager.clone();
    assert!(
        eventually(move || {
            let manager = manager.clone();
            Box::pin(async move { !manager.has_checker(session).await })
        })
        .await,
        "the checker closes with the app"
    );
}

/// A replaced app's checker closes too; the next check starts a fresh one.
#[tokio::test]
async fn a_replaced_app_closes_the_old_checker() {
    let Some(rig) = rig().await else { return };
    let session = rig.ctx.session_id.unwrap();
    assert_eq!(
        rig.check(json!({"pages": [{"path": "/"}]})).await.exit_code,
        Some(0)
    );
    let other = support::app::app().await;
    rig.ctx.web_app.set(RunningApp {
        generation: 2,
        command: "npm run dev".into(),
        port: 5173,
        origin: other.origin,
    });
    let manager = rig.manager.clone();
    assert!(
        eventually(move || {
            let manager = manager.clone();
            Box::pin(async move { !manager.has_checker(session).await })
        })
        .await,
        "the old app's checker closes"
    );
    assert_eq!(
        rig.check(json!({"pages": [{"path": "/"}]})).await.exit_code,
        Some(0)
    );
    assert!(rig.manager.has_checker(session).await);
    rig.manager.close_all().await;
}

/// The session retiring shuts the tools down: its checkers go with it.
#[tokio::test]
async fn retiring_the_session_closes_its_checkers() {
    let Some(rig) = rig().await else { return };
    let session = rig.ctx.session_id.unwrap();
    assert_eq!(
        rig.check(json!({"pages": [{"path": "/"}]})).await.exit_code,
        Some(0)
    );
    rig.tool.shutdown().await;
    assert!(!rig.manager.has_checker(session).await);
}

/// An unused checker browser is closed after a while; the next check starts
/// another.
#[tokio::test]
async fn an_idle_checker_is_closed() {
    let Some(executable) = support::test_browser() else {
        return;
    };
    let manager = WebCheckManager::new(
        WebCheckConfig { enabled: true },
        BrowserConfig {
            executable: Some(executable.display().to_string()),
            ..BrowserConfig::default()
        },
        WebConfig::default(),
        WebCheckOptions {
            extra_args: support::test_browser_args(),
            require_relay: false,
            idle_close: std::time::Duration::from_millis(300),
            ..WebCheckOptions::default()
        },
    );
    let tool = web_tools(manager.clone())
        .into_iter()
        .find(|tool| tool.name() == "web_check")
        .unwrap();
    let app = support::app::app().await;
    let workspace = tempfile::tempdir().unwrap();
    let session = SessionId::new_v4();
    let ctx = ToolContext::new(workspace.path().to_path_buf()).with_session_id(session);
    ctx.web_app.set(RunningApp {
        generation: 1,
        command: "npm run dev".into(),
        port: 5173,
        origin: app.origin,
    });
    let output = tool
        .call(&ctx, json!({"pages": [{"path": "/"}]}))
        .await
        .unwrap();
    assert_eq!(output.exit_code, Some(0), "{}", output.content);
    assert!(manager.has_checker(session).await);
    let closing = manager.clone();
    assert!(
        eventually(move || {
            let manager = closing.clone();
            Box::pin(async move { !manager.has_checker(session).await })
        })
        .await,
        "an idle checker is closed"
    );
    let output = tool
        .call(&ctx, json!({"pages": [{"path": "/"}]}))
        .await
        .unwrap();
    assert_eq!(
        output.exit_code,
        Some(0),
        "the next check starts another: {}",
        output.content
    );
    manager.close_all().await;
}
