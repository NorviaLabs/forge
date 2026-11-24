//! Browser control end to end: a real Chromium-family browser driven
//! through the tools, against a fixture site on loopback.
//!
//! Needs a browser (`FORGE_TEST_CHROMIUM` or one on `PATH`); without one the
//! tests say they were skipped, unless `FORGE_REQUIRE_COMPUTER_TESTS=1`.

mod support;

use std::net::SocketAddr;
use std::sync::Arc;

use forge_browser::{browser_tools, BrowserManager, ManagerOptions};
use forge_config::BrowserConfig;
use forge_tools::{ConsentKind, SessionTempDir, Tool, ToolContext, ToolError};
use forge_types::{SessionId, ToolOutput};
use serde_json::{json, Value};

struct Rig {
    manager: Arc<BrowserManager>,
    tools: Vec<Arc<dyn Tool>>,
    site: SocketAddr,
    session: SessionId,
    consents: Arc<forge_tools::SessionConsents>,
    workspace: tempfile::TempDir,
    session_tmp: Arc<SessionTempDir>,
    egress: Option<Arc<forge_tools::sandbox::EgressGrant>>,
}

async fn rig_with(require_egress: bool) -> Option<Rig> {
    let executable = support::test_browser()?;
    let manager = BrowserManager::new(
        BrowserConfig {
            enabled: true,
            executable: Some(executable.display().to_string()),
            headless: true,
        },
        ManagerOptions {
            extra_args: support::test_browser_args(),
            require_egress,
            ..ManagerOptions::default()
        },
    );
    let session = SessionId::new_v4();
    Some(Rig {
        tools: browser_tools(manager.clone()),
        manager,
        site: support::fixture_site().await,
        session,
        consents: forge_tools::SessionConsents::new(),
        workspace: tempfile::tempdir().unwrap(),
        session_tmp: SessionTempDir::create(session).unwrap(),
        egress: None,
    })
}

async fn rig() -> Option<Rig> {
    rig_with(false).await
}

impl Rig {
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.site)
    }

    fn ctx(&self, call: &str) -> ToolContext {
        let mut ctx = ToolContext::new(self.workspace.path().to_path_buf())
            .with_session_id(self.session)
            .with_session_tmp(self.session_tmp.clone())
            .with_tool_call_id(call);
        ctx.image_input = true;
        ctx.consents = self.consents.clone();
        ctx.egress = self.egress.clone();
        ctx
    }

    async fn try_call(&self, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("no tool {name}"));
        tool.call(&self.ctx(&format!("{name}-call")), args).await
    }

    async fn call(&self, name: &str, args: Value) -> ToolOutput {
        self.try_call(name, args)
            .await
            .unwrap_or_else(|error| panic!("{name} failed: {error}"))
    }

    async fn ok(&self, name: &str, args: Value) -> String {
        let output = self.call(name, args).await;
        assert!(!output.is_error, "{name}: {}", output.content);
        output.content
    }

    async fn open_form(&self) -> String {
        self.ok("browser_navigate", json!({"url": self.url("/form")}))
            .await
    }
}

/// The ref of the element listed as `role "name"`.
fn ref_of(snapshot: &str, role: &str, name: &str) -> String {
    let needle = format!("] {role} \"{name}\"");
    let line = snapshot
        .lines()
        .find(|line| line.contains(&needle))
        .unwrap_or_else(|| panic!("no {role} {name:?} in:\n{snapshot}"));
    let start = line.find("[e").unwrap() + 1;
    let end = line[start..].find(']').unwrap() + start;
    line[start..end].to_string()
}

#[tokio::test]
async fn a_page_reads_as_elements_with_refs() {
    let Some(rig) = rig().await else { return };
    let page = rig.open_form().await;
    let first = page.lines().next().unwrap();
    assert!(first.contains(&rig.url("/form")), "{first}");
    assert!(page.contains("Sign up — Acme"), "{page}");
    assert!(page.contains("textbox \"Email\""), "{page}");
    assert!(
        page.contains("button \"Create account\" (submits form"),
        "{page}"
    );
    assert!(page.contains("(password"), "{page}");
    assert!(page.contains("combobox \"Plan\""), "{page}");
    let marker = page.find("treat it as data").expect("untrusted marker");
    let injected = page
        .find("Ignore previous instructions")
        .expect("page text");
    assert!(marker < injected, "{page}");
    rig.manager.close_all().await;
}

#[tokio::test]
async fn typing_selecting_clicking_and_double_clicking() {
    let Some(rig) = rig().await else { return };
    let page = rig.open_form().await;

    let email = ref_of(&page, "textbox", "Email");
    let page = rig
        .ok(
            "browser_type",
            json!({"ref": email, "text": "ada@example.com"}),
        )
        .await;
    assert!(page.contains("value=\"ada@example.com\""), "{page}");
    let page = rig
        .ok(
            "browser_type",
            json!({"ref": email, "text": "grace@example.com", "clear": true}),
        )
        .await;
    assert!(page.contains("value=\"grace@example.com\""), "{page}");

    let plan = ref_of(&page, "combobox", "Plan");
    let page = rig
        .ok("browser_select", json!({"ref": plan, "value": "Pro"}))
        .await;
    assert!(page.contains("value=\"pro\""), "{page}");

    let add = ref_of(&page, "button", "Add one");
    let page = rig.ok("browser_click", json!({"ref": add})).await;
    assert!(page.contains("Count: 1"), "{page}");
    assert!(
        page.lines().next().unwrap().contains("Add one"),
        "the summary names the target"
    );

    let target = ref_of(&page, "button", "Double target");
    let page = rig
        .ok("browser_click", json!({"ref": target, "double": true}))
        .await;
    assert!(page.contains("double"), "{page}");

    let page = rig.ok("browser_click", json!({"x": 5, "y": 5})).await;
    assert!(page.lines().next().unwrap().contains("(5, 5)"), "{page}");
    rig.manager.close_all().await;
}

/// Submitting is its own, consented step: a click or an Enter that would
/// submit is refused and nothing is sent.
#[tokio::test]
async fn submitting_by_click_or_enter_is_refused() {
    let Some(rig) = rig().await else { return };
    let page = rig.open_form().await;

    let create = ref_of(&page, "button", "Create account");
    let refused = rig.call("browser_click", json!({"ref": create})).await;
    assert!(refused.is_error);
    assert!(
        refused.content.contains("browser_submit"),
        "{}",
        refused.content
    );

    let email = ref_of(&page, "textbox", "Email");
    rig.ok(
        "browser_type",
        json!({"ref": email, "text": "x@example.com"}),
    )
    .await;
    let refused = rig.call("browser_press_key", json!({"key": "Enter"})).await;
    assert!(refused.is_error);
    assert!(
        refused.content.contains("browser_submit"),
        "{}",
        refused.content
    );

    let page = rig.ok("browser_snapshot", json!({})).await;
    assert!(page.contains("Sign up — Acme"), "still on the form: {page}");
    rig.manager.close_all().await;
}

#[tokio::test]
async fn passwords_are_for_the_user_to_type() {
    let Some(rig) = rig().await else { return };
    let page = rig.open_form().await;
    let password = ref_of(&page, "textbox", "Password");
    let refused = rig
        .call("browser_type", json!({"ref": password, "text": "hunter2"}))
        .await;
    assert!(refused.is_error);
    assert!(
        refused.content.contains("ask the user"),
        "{}",
        refused.content
    );
    rig.manager.close_all().await;
}

/// Typing a password key by key is typing a password.
#[tokio::test]
async fn a_password_cannot_be_typed_key_by_key() {
    let Some(rig) = rig().await else { return };
    let page = rig.open_form().await;
    let password = ref_of(&page, "textbox", "Password");
    rig.ok("browser_click", json!({"ref": password})).await;
    let refused = rig.call("browser_press_key", json!({"key": "h"})).await;
    assert!(refused.is_error);
    assert!(refused.content.contains("password"), "{}", refused.content);
    // Moving on from the field is fine.
    rig.ok("browser_press_key", json!({"key": "Tab"})).await;
    rig.manager.close_all().await;
}

/// A click lands where the element is; when something else is on top
/// (a transparent cover, another site's frame), the click is refused.
#[tokio::test]
async fn a_covered_element_is_not_clicked() {
    let Some(rig) = rig().await else { return };
    let page = rig
        .ok("browser_navigate", json!({"url": rig.url("/overlay")}))
        .await;
    let under = ref_of(&page, "button", "Harmless");
    let refused = rig.call("browser_click", json!({"ref": under})).await;
    assert!(refused.is_error);
    assert!(refused.content.contains("covered"), "{}", refused.content);
    let page = rig.ok("browser_snapshot", json!({})).await;
    assert!(
        page.contains("Page: Covered"),
        "nothing was clicked: {page}"
    );
    rig.manager.close_all().await;
}

/// A grant covers where the data goes, and the card says when that is
/// another site.
#[tokio::test]
async fn a_cross_site_submit_names_the_other_site() {
    let Some(rig) = rig().await else { return };
    let page = rig
        .ok("browser_navigate", json!({"url": rig.url("/cross-site")}))
        .await;
    let subscribe = ref_of(&page, "button", "Subscribe");
    rig.consents
        .grant(ConsentKind::FormSubmit, &rig.site.to_string());
    let error = rig
        .try_call("browser_submit", json!({"ref": subscribe}))
        .await
        .unwrap_err();
    let ToolError::ConsentRequired(request) = error else {
        panic!("a grant for this site does not cover another: {error}");
    };
    assert_eq!(request.scope, "collect.invalid");
    assert!(
        request
            .summary
            .contains("Sends to another site: collect.invalid"),
        "{}",
        request.summary
    );
    rig.manager.close_all().await;
}

#[tokio::test]
async fn a_submit_asks_then_sends_the_form() {
    let Some(rig) = rig().await else { return };
    let page = rig.open_form().await;
    let email = ref_of(&page, "textbox", "Email");
    rig.ok(
        "browser_type",
        json!({"ref": email, "text": "ada@example.com"}),
    )
    .await;
    let name = ref_of(&page, "textbox", "Full name");
    rig.ok("browser_type", json!({"ref": name, "text": "Ada"}))
        .await;
    let plan = ref_of(&page, "combobox", "Plan");
    rig.ok("browser_select", json!({"ref": plan, "value": "pro"}))
        .await;
    let create = ref_of(&page, "button", "Create account");

    let error = rig
        .try_call("browser_submit", json!({"ref": create}))
        .await
        .unwrap_err();
    let ToolError::ConsentRequired(request) = error else {
        panic!("expected a consent request, got {error}");
    };
    assert_eq!(request.kind, ConsentKind::FormSubmit);
    assert_eq!(request.scope, rig.site.to_string());
    assert!(
        request.summary.contains("Create account"),
        "{}",
        request.summary
    );
    assert!(
        request.summary.contains("email = ada@example.com"),
        "{}",
        request.summary
    );
    assert!(request.summary.contains("POST"), "{}", request.summary);

    rig.consents.grant(ConsentKind::FormSubmit, &request.scope);
    let page = rig.ok("browser_submit", json!({"ref": create})).await;
    assert!(page.contains("Welcome"), "{page}");
    assert!(
        page.contains("email=ada@example.com;name=Ada;plan=pro"),
        "{page}"
    );
    rig.manager.close_all().await;
}

#[tokio::test]
async fn scrolling_brings_the_rest_of_the_page_into_view() {
    let Some(rig) = rig().await else { return };
    let page = rig
        .ok("browser_navigate", json!({"url": rig.url("/scroll")}))
        .await;
    assert!(!page.contains("bottom"), "not visible yet: {page}");
    let page = rig.ok("browser_scroll", json!({"dy": 5000})).await;
    assert!(page.contains("bottom"), "{page}");
    rig.manager.close_all().await;
}

#[tokio::test]
async fn a_screenshot_is_an_image_attachment() {
    let Some(rig) = rig().await else { return };
    rig.open_form().await;
    let output = rig.call("browser_screenshot", json!({})).await;
    assert!(!output.is_error, "{}", output.content);
    let image = output.attachments.first().expect("an image");
    let bytes = std::fs::read(&image.path).unwrap();
    assert_eq!(&bytes[..4], b"\x89PNG");
    assert_eq!(image.width, Some(1280));
    // Not where sandboxed commands can write: they could swap the folder
    // for a link and have Forge write wherever the user can.
    assert!(
        !std::path::Path::new(&image.path).starts_with(rig.session_tmp.path()),
        "screenshots stay out of the session's temp dir"
    );
    rig.manager.close_all().await;
}

#[tokio::test]
async fn new_tabs_are_tracked_and_can_be_switched_and_closed() {
    let Some(rig) = rig().await else { return };
    let page = rig.open_form().await;
    let popup = ref_of(&page, "link", "Open in new tab");
    let output = rig.ok("browser_click", json!({"ref": popup})).await;
    assert!(output.contains("new tab"), "{output}");

    let tabs = rig.ok("browser_tabs", json!({"action": "list"})).await;
    assert!(tabs.contains("2 tabs"), "{tabs}");
    rig.ok("browser_tabs", json!({"action": "switch", "index": 0}))
        .await;
    let tabs = rig
        .ok("browser_tabs", json!({"action": "close", "index": 1}))
        .await;
    assert!(tabs.contains("1 tab"), "{tabs}");
    rig.manager.close_all().await;
}

#[tokio::test]
async fn dialogs_are_dismissed_and_reported() {
    let Some(rig) = rig().await else { return };
    let page = rig.open_form().await;
    let hello = ref_of(&page, "button", "Say hello");
    let output = rig.ok("browser_click", json!({"ref": hello})).await;
    assert!(output.contains("dialog \"hello there\""), "{output}");
    rig.manager.close_all().await;
}

#[tokio::test]
async fn history_goes_back_and_forward() {
    let Some(rig) = rig().await else { return };
    rig.open_form().await;
    rig.ok("browser_navigate", json!({"url": rig.url("/scroll")}))
        .await;
    let back = rig.ok("browser_navigate", json!({"action": "back"})).await;
    assert!(back.contains("Sign up — Acme"), "{back}");
    let forward = rig
        .ok("browser_navigate", json!({"action": "forward"}))
        .await;
    assert!(forward.contains("Long page"), "{forward}");
    let reload = rig
        .ok("browser_navigate", json!({"action": "reload"}))
        .await;
    assert!(reload.contains("Long page"), "{reload}");
    rig.manager.close_all().await;
}

#[tokio::test]
async fn a_stale_ref_asks_for_a_new_snapshot() {
    let Some(rig) = rig().await else { return };
    rig.open_form().await;
    let output = rig.call("browser_click", json!({"ref": "e999"})).await;
    assert!(output.is_error);
    assert!(
        output.content.contains("browser_snapshot"),
        "{}",
        output.content
    );
    rig.manager.close_all().await;
}

/// Through the session gateway: an allowed host loads, any other is refused
/// as a denied host, which the session turns into the host-grant prompt.
#[tokio::test]
async fn traffic_follows_the_session_network_policy() {
    let Some(mut rig) = rig_with(true).await else {
        return;
    };
    let mut policy = forge_tools::egress::EgressPolicy::new();
    policy.allow("127.0.0.1");
    let shared = forge_tools::egress::EgressShared::new(policy, Default::default());
    rig.egress = Some(Arc::new(forge_tools::sandbox::EgressGrant {
        proxy_port: 0,
        socket_path: rig.workspace.path().join("unused.sock"),
        control: Some(shared),
    }));

    let page = rig.open_form().await;
    assert!(page.contains("Sign up — Acme"), "{page}");

    let error = rig
        .try_call(
            "browser_navigate",
            json!({"url": "http://blocked.invalid/"}),
        )
        .await
        .unwrap_err();
    match error {
        ToolError::SandboxDenied { denied_host, .. } => {
            assert_eq!(denied_host.as_deref(), Some("blocked.invalid"));
        }
        other => panic!("expected a denied host, got {other}"),
    }
    rig.manager.close_all().await;
}

#[tokio::test]
async fn the_browser_is_reported_and_closed_with_its_processes() {
    let Some(rig) = rig().await else { return };
    assert!(
        rig.manager.status(rig.session).is_none(),
        "nothing runs before first use"
    );
    rig.open_form().await;

    let status = rig.manager.status(rig.session).expect("a browser runs");
    assert_eq!(status.url, rig.url("/form"));
    assert_eq!(status.title, "Sign up — Acme");
    assert_eq!(status.tabs, 1);
    assert!(!status.browser.is_empty());

    let pid = rig.manager.browser_pid(rig.session).unwrap() as i32;
    assert!(alive(pid));
    assert!(rig.manager.close(rig.session).await);
    assert!(rig.manager.status(rig.session).is_none());
    assert!(!alive(pid), "the browser process is gone");
    assert!(!rig.manager.close(rig.session).await);

    // Shutting the tools down (the session is retired) closes browsers too.
    rig.open_form().await;
    let pid = rig.manager.browser_pid(rig.session).unwrap() as i32;
    rig.tools[0].shutdown().await;
    assert!(!alive(pid));
}

fn alive(pid: i32) -> bool {
    // Reap it if it is our exited child, then ask whether it still exists.
    unsafe {
        libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG);
        libc::kill(pid, 0) == 0
    }
}

/// Nothing waits for a page that is not loading. Each snapshot and action
/// used to sleep a fixed 150 ms first: ten snapshots cost at least 1.5 s of
/// sleep alone, ten clicks at least 1.9 s. Bounds are generous for a loaded
/// machine and still well below that.
#[tokio::test]
async fn reading_and_acting_do_not_wait_for_nothing() {
    let Some(rig) = rig().await else { return };
    let page = rig.open_form().await;
    let add = ref_of(&page, "button", "Add one");

    let started = std::time::Instant::now();
    for _ in 0..10 {
        rig.ok("browser_snapshot", json!({})).await;
    }
    let snapshots = started.elapsed();

    let started = std::time::Instant::now();
    for _ in 0..10 {
        rig.ok("browser_click", json!({"ref": add})).await;
    }
    let clicks = started.elapsed();
    let page = rig.ok("browser_snapshot", json!({})).await;
    assert!(page.contains("Count: 10"), "every click landed: {page}");

    assert!(
        snapshots < std::time::Duration::from_millis(1000),
        "10 snapshots took {snapshots:?}"
    );
    assert!(
        clicks < std::time::Duration::from_millis(1800),
        "10 clicks took {clicks:?}"
    );
    rig.manager.close_all().await;
}

/// Reading a long page costs what is on screen, not the whole page: a
/// 40,000-row page is read quickly, and what is on screen is all there
/// (a fixed button included), at the top and after scrolling to the end.
#[tokio::test]
async fn a_long_page_is_read_by_what_is_on_screen() {
    let Some(rig) = rig().await else { return };
    rig.ok("browser_navigate", json!({"url": rig.url("/big")}))
        .await;

    let started = std::time::Instant::now();
    let page = rig.ok("browser_snapshot", json!({})).await;
    let took = started.elapsed();
    assert!(page.contains("link \"Link number 0\""), "{page}");
    assert!(page.contains("text for row 0 with several words"), "{page}");
    assert!(page.contains("button \"Help\""), "the fixed button: {page}");
    assert!(!page.contains("Link number 39999"), "far below the screen");
    // The regression this guards was 2.9 s; a second leaves room for a busy
    // machine (541 ms was seen with the whole workspace testing at once).
    assert!(
        took < std::time::Duration::from_secs(1),
        "one snapshot took {took:?}"
    );

    let page = rig.ok("browser_scroll", json!({"dy": 100_000_000})).await;
    assert!(page.contains("link \"Link number 39999\""), "{page}");
    assert!(page.contains("text for row 39999"), "{page}");
    assert!(page.contains("button \"Help\""), "the fixed button: {page}");
    assert!(!page.contains("Link number 0\""), "far above the screen");
    rig.manager.close_all().await;
}
