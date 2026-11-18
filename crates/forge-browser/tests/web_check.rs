//! Web check end to end: a real headless browser checks pages of a fixture
//! app that is served on a Unix socket behind the real ingress relay, the
//! way a sandboxed app is reached.
//!
//! Needs a browser (`FORGE_TEST_CHROMIUM` or one on `PATH`); without one the
//! tests say they were skipped, unless `FORGE_REQUIRE_COMPUTER_TESTS=1`.

#[allow(dead_code)]
mod support;

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::Duration;

use forge_browser::checker::{
    CheckerBrowser, Expect, PageReport, PageSpec, Screenshot, Step, Viewport, DESKTOP, MOBILE,
};
use support::app::app;

async fn checker(origin: SocketAddr) -> Option<CheckerBrowser> {
    let executable = support::test_browser()?;
    Some(
        CheckerBrowser::start(&executable, origin, support::test_browser_args())
            .await
            .expect("the checker starts"),
    )
}

fn spec(path: &str) -> PageSpec {
    PageSpec {
        path: path.into(),
        ..PageSpec::default()
    }
}

async fn check(browser: &mut CheckerBrowser, page: PageSpec, viewport: Viewport) -> PageReport {
    tokio::time::timeout(
        Duration::from_secs(60),
        browser.check(&page, viewport, Screenshot::Never),
    )
    .await
    .expect("a check finishes")
}

#[tokio::test]
async fn a_healthy_page_passes_with_its_expectations() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    assert!(browser.is_alive());
    let report = check(
        &mut browser,
        PageSpec {
            path: "/".into(),
            expect: Some(Expect {
                text: Some("Welcome".into()),
                selector: Some("a[href='/settings']".into()),
                title: Some("Home".into()),
            }),
            steps: vec![],
        },
        DESKTOP,
    )
    .await;
    assert_eq!(report.status, Some(200), "{report:?}");
    assert!(report.passed(), "{report:?}");
    assert_eq!(report.expectations.len(), 3, "{report:?}");
    assert!(report.expectations.iter().all(|e| e.ok), "{report:?}");
    assert!(report.console_errors.is_empty() && report.exceptions.is_empty());
    browser.kill().await;
}

#[tokio::test]
async fn an_uncaught_exception_fails_the_page() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    let report = check(&mut browser, spec("/throws"), DESKTOP).await;
    assert!(!report.passed(), "{report:?}");
    assert_eq!(report.exceptions.len(), 2, "{report:?}");
    assert!(
        report
            .exceptions
            .iter()
            .any(|e| e.contains("boom-exception")),
        "{report:?}"
    );
    assert!(
        report.exceptions.iter().any(|e| e.contains("classList")),
        "{report:?}"
    );
    browser.kill().await;
}

#[tokio::test]
async fn a_console_error_fails_the_page_and_a_warning_does_not_count() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    let report = check(&mut browser, spec("/console"), DESKTOP).await;
    assert!(!report.passed(), "{report:?}");
    assert_eq!(report.console_errors, ["console boom 42"], "{report:?}");
    browser.kill().await;
}

/// A missing asset is one failed request, not also a console error.
#[tokio::test]
async fn a_missing_asset_is_a_failed_request() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    let report = check(&mut browser, spec("/missing-asset"), DESKTOP).await;
    assert!(!report.passed(), "{report:?}");
    assert_eq!(
        report.failed_requests,
        ["GET /nope.png → 404"],
        "{report:?}"
    );
    assert!(report.console_errors.is_empty(), "{report:?}");
    browser.kill().await;
}

#[tokio::test]
async fn a_server_error_fails_the_page() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    let report = check(&mut browser, spec("/500"), DESKTOP).await;
    assert_eq!(report.status, Some(500), "{report:?}");
    assert!(!report.passed());
    assert!(
        report.failed_requests.is_empty(),
        "the page itself: {report:?}"
    );
    browser.kill().await;
}

/// Outside hosts are refused and reported, and do not fail the check.
#[tokio::test]
async fn outside_hosts_are_blocked_and_not_counted() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    let report = check(&mut browser, spec("/external"), DESKTOP).await;
    assert!(report.passed(), "{report:?}");
    let hosts: Vec<&str> = report.blocked.iter().map(|b| b.host.as_str()).collect();
    assert!(hosts.contains(&"fonts.googleapis.com"), "{report:?}");
    assert!(hosts.contains(&"example.com"), "{report:?}");
    assert!(report.failed_requests.is_empty(), "{report:?}");
    assert!(report.console_errors.is_empty(), "{report:?}");
    browser.kill().await;
}

/// The page cannot reach other services on the host's loopback, by address
/// or by name: the app's origin is the only way out of the browser.
#[tokio::test]
async fn other_loopback_services_are_unreachable() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    let report = check(&mut browser, spec("/loopback"), DESKTOP).await;
    assert_eq!(app.other_hits.load(Ordering::SeqCst), 0, "{report:?}");
    let hosts: Vec<&str> = report.blocked.iter().map(|b| b.host.as_str()).collect();
    let by_address = format!("127.0.0.1:{}", app.other_port);
    let by_name = format!("localhost:{}", app.other_port);
    assert!(hosts.contains(&by_address.as_str()), "{report:?}");
    assert!(hosts.contains(&by_name.as_str()), "{report:?}");
    assert!(report.passed(), "{report:?}");
    browser.kill().await;
}

#[tokio::test]
async fn steps_type_click_and_press() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    let expect_text = |text: &str| {
        Some(Expect {
            text: Some(text.into()),
            ..Expect::default()
        })
    };
    let report = check(
        &mut browser,
        PageSpec {
            path: "/form".into(),
            expect: None,
            steps: vec![
                Step {
                    type_into: Some("#name".into()),
                    text: Some("Ada".into()),
                    ..Step::default()
                },
                Step {
                    click: Some("text=Greet".into()),
                    expect: expect_text("Hello, Ada"),
                    ..Step::default()
                },
                Step {
                    type_into: Some("text=Name".into()),
                    text: Some("Bob".into()),
                    ..Step::default()
                },
                Step {
                    press: Some("Enter".into()),
                    expect: expect_text("Hello, Bob"),
                    ..Step::default()
                },
            ],
        },
        DESKTOP,
    )
    .await;
    assert!(report.passed(), "{report:?}");
    assert_eq!(report.steps.len(), 4);
    assert!(report.steps.iter().all(|step| step.error.is_none()));
    browser.kill().await;
}

#[tokio::test]
async fn an_unmet_expectation_fails_and_says_what_was_expected() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    let report = check(
        &mut browser,
        PageSpec {
            path: "/".into(),
            expect: Some(Expect {
                text: Some("Nonexistent words".into()),
                selector: Some("#missing".into()),
                title: Some("Other title".into()),
            }),
            steps: vec![],
        },
        DESKTOP,
    )
    .await;
    assert!(!report.passed());
    assert_eq!(report.expectations.len(), 3);
    assert!(report.expectations.iter().all(|e| !e.ok), "{report:?}");
    assert!(report.expectations[0].what.contains("Nonexistent words"));
    assert!(
        report.expectations[2]
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("Home — Acme"),
        "the actual title is reported: {report:?}"
    );
    browser.kill().await;
}

/// A step that cannot be done fails the page; the steps after it are not
/// run.
#[tokio::test]
async fn a_step_without_a_target_stops_the_steps() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    let report = check(
        &mut browser,
        PageSpec {
            path: "/form".into(),
            expect: None,
            steps: vec![
                Step {
                    click: Some("text=Nope".into()),
                    ..Step::default()
                },
                Step {
                    click: Some("#greet".into()),
                    ..Step::default()
                },
            ],
        },
        DESKTOP,
    )
    .await;
    assert!(!report.passed());
    assert_eq!(report.steps.len(), 1, "{report:?}");
    assert!(
        report.steps[0]
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Nope"),
        "{report:?}"
    );
    assert_eq!(report.skipped_steps, 1);
    browser.kill().await;
}

/// Content that arrives after the load event is waited for.
#[tokio::test]
async fn content_loaded_after_the_page_is_waited_for() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    let report = check(
        &mut browser,
        PageSpec {
            path: "/late".into(),
            expect: Some(Expect {
                text: Some("Loaded data".into()),
                ..Expect::default()
            }),
            steps: vec![],
        },
        DESKTOP,
    )
    .await;
    assert!(report.passed(), "{report:?}");
    browser.kill().await;
}

#[tokio::test]
async fn the_mobile_viewport_is_a_phone() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    let menu = PageSpec {
        path: "/mobile".into(),
        expect: Some(Expect {
            text: Some("Mobile menu".into()),
            ..Expect::default()
        }),
        steps: vec![],
    };
    let phone = tokio::time::timeout(
        Duration::from_secs(60),
        browser.check(&menu, MOBILE, Screenshot::Always),
    )
    .await
    .unwrap();
    assert!(phone.passed(), "{phone:?}");
    let shot =
        forge_types::inspect_image(phone.screenshot.as_deref().expect("a screenshot")).unwrap();
    assert_eq!((shot.width, shot.height), (Some(390), Some(844)));

    let desktop = check(&mut browser, menu, DESKTOP).await;
    assert!(!desktop.passed(), "hidden on a wide screen: {desktop:?}");
    assert!(desktop.screenshot.is_none(), "not asked for");
    browser.kill().await;
}

#[tokio::test]
async fn a_desktop_screenshot_is_the_viewport() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    let report = tokio::time::timeout(
        Duration::from_secs(60),
        browser.check(&spec("/"), DESKTOP, Screenshot::Always),
    )
    .await
    .unwrap();
    let shot =
        forge_types::inspect_image(report.screenshot.as_deref().expect("a screenshot")).unwrap();
    assert_eq!((shot.width, shot.height), (Some(1280), Some(800)));
    browser.kill().await;
}

/// An app that stopped answering is reported, not waited on forever.
#[tokio::test]
async fn an_app_that_does_not_answer_is_reported() {
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let Some(mut browser) = checker(closed).await else {
        return;
    };
    let report = check(&mut browser, spec("/"), DESKTOP).await;
    assert!(!report.passed());
    assert!(report.load_error.is_some(), "{report:?}");
    browser.kill().await;
}

/// A dialog does not hold the check up.
#[tokio::test]
async fn a_dialog_is_dismissed() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    let report = check(
        &mut browser,
        PageSpec {
            path: "/alert".into(),
            expect: Some(Expect {
                text: Some("after the alert".into()),
                ..Expect::default()
            }),
            steps: vec![],
        },
        DESKTOP,
    )
    .await;
    assert!(report.passed(), "{report:?}");
    browser.kill().await;
}

/// A second check on the same browser starts clean.
#[tokio::test]
async fn each_check_starts_clean() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    let first = check(&mut browser, spec("/console"), DESKTOP).await;
    assert!(!first.passed());
    let second = check(&mut browser, spec("/"), DESKTOP).await;
    assert!(second.passed(), "{second:?}");
    browser.kill().await;
}

/// A step that starts no request goes on at once instead of waiting out a
/// network-quiet window.
#[tokio::test]
async fn steps_without_network_do_not_wait_for_it() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    check(&mut browser, spec("/form"), DESKTOP).await; // warm
    let started = std::time::Instant::now();
    check(&mut browser, spec("/form"), DESKTOP).await;
    let plain = started.elapsed();
    let steps = PageSpec {
        path: "/form".into(),
        expect: None,
        steps: vec![
            Step {
                type_into: Some("#name".into()),
                text: Some("Ada".into()),
                ..Step::default()
            },
            Step {
                click: Some("text=Greet".into()),
                ..Step::default()
            },
        ],
    };
    let started = std::time::Instant::now();
    let report = check(&mut browser, steps, DESKTOP).await;
    let with_steps = started.elapsed();
    assert!(report.passed(), "{report:?}");
    let added = with_steps.saturating_sub(plain);
    assert!(
        added < Duration::from_millis(600),
        "two quiet steps added {added:?}"
    );
    browser.kill().await;
}

/// A step that does start a request is waited for: its failure is in the
/// report.
#[tokio::test]
async fn a_step_that_starts_a_request_waits_for_it() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    let report = check(
        &mut browser,
        PageSpec {
            path: "/click-fetch".into(),
            expect: None,
            steps: vec![Step {
                click: Some("text=Load".into()),
                ..Step::default()
            }],
        },
        DESKTOP,
    )
    .await;
    assert_eq!(
        report.failed_requests,
        ["GET /slow-missing → 404"],
        "{report:?}"
    );
    browser.kill().await;
}

/// A screenshot taken only if the page fails: none for a page that passes,
/// one for a page that does not.
#[tokio::test]
async fn a_screenshot_can_be_kept_for_a_failure() {
    let app = app().await;
    let Some(mut browser) = checker(app.origin).await else {
        return;
    };
    let passed = browser
        .check(&spec("/"), DESKTOP, Screenshot::IfFailed)
        .await;
    assert!(passed.passed());
    assert!(
        passed.screenshot.is_none(),
        "no picture of a page that passed"
    );
    let failed = browser
        .check(&spec("/throws"), DESKTOP, Screenshot::IfFailed)
        .await;
    assert!(!failed.passed());
    assert!(failed.screenshot.is_some(), "a picture of the failure");
    browser.kill().await;
}
