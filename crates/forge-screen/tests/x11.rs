//! The X11 backend against a private Xvfb display.
//!
//! Each test starts its own Xvfb, so nothing here touches the user's
//! screen. Without Xvfb the tests say so and pass, unless
//! `FORGE_REQUIRE_COMPUTER_TESTS=1` (set in CI) makes that a failure.
#![cfg(target_os = "linux")]

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use forge_screen::backend::{Button, Point, Rect, ScreenBackend};
use forge_screen::keys::KeyChord;
use forge_screen::linux_x11::X11Backend;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, EventMask,
    InputFocus, PropMode, Window, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

struct Xvfb {
    child: Child,
    display: String,
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn xvfb() -> Option<Xvfb> {
    xvfb_with(&[])
}

fn xvfb_with(extra: &[&str]) -> Option<Xvfb> {
    let required = std::env::var("FORGE_REQUIRE_COMPUTER_TESTS").as_deref() == Ok("1");
    // Distinct per test in this process, spread across processes by pid.
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let base = 200 + (std::process::id() % 100) * 16;
    for _ in 0..40 {
        let number = base + NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst) % 1600;
        if std::path::Path::new(&format!("/tmp/.X11-unix/X{number}")).exists()
            || std::path::Path::new(&format!("/tmp/.X{number}-lock")).exists()
        {
            continue;
        }
        let display = format!(":{number}");
        let child = match Command::new("Xvfb")
            .args([
                display.as_str(),
                "-screen",
                "0",
                "1024x768x24",
                "-nolisten",
                "tcp",
                "-noreset",
            ])
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                assert!(!required, "Xvfb is required here: {error}");
                eprintln!("skipped: Xvfb is not installed ({error})");
                return None;
            }
        };
        let mut server = Xvfb { child, display };
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = server.child.try_wait() {
                break; // lost a race for this display number
            }
            if RustConnection::connect(Some(&server.display)).is_ok() {
                // Ours, not another process's: it is still running.
                if let Ok(None) = server.child.try_wait() {
                    return Some(server);
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    assert!(!required, "could not start Xvfb");
    eprintln!("skipped: could not start Xvfb");
    None
}

/// A mapped window owned by this test, selecting pointer and key events.
struct TestWindow {
    conn: RustConnection,
    window: Window,
}

fn test_window(display: &str, x: i16, y: i16, width: u16, height: u16, pixel: u32) -> TestWindow {
    let (conn, screen_num) = RustConnection::connect(Some(display)).unwrap();
    let screen = &conn.setup().roots[screen_num];
    let window = conn.generate_id().unwrap();
    conn.create_window(
        screen.root_depth,
        window,
        screen.root,
        x,
        y,
        width,
        height,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .background_pixel(pixel)
            .override_redirect(1)
            .event_mask(
                EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::KEY_PRESS
                    | EventMask::KEY_RELEASE
                    | EventMask::EXPOSURE,
            ),
    )
    .unwrap();
    conn.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        b"forge test window",
    )
    .unwrap();
    conn.map_window(window).unwrap();
    conn.flush().unwrap();
    // Wait until it is on screen.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(Some(Event::Expose(_))) = conn.poll_for_event() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    TestWindow { conn, window }
}

impl TestWindow {
    /// Events that arrive within `wait`.
    fn events(&self, wait: Duration) -> Vec<Event> {
        let deadline = Instant::now() + wait;
        let mut events = Vec::new();
        while Instant::now() < deadline {
            match self.conn.poll_for_event().unwrap() {
                Some(event) => events.push(event),
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        events
    }

    fn focus(&self) {
        self.conn
            .set_input_focus(InputFocus::POINTER_ROOT, self.window, x11rb::CURRENT_TIME)
            .unwrap();
        self.conn.flush().unwrap();
        self.conn.sync().unwrap();
    }

    fn keysym_of(&self, keycode: u8) -> u32 {
        let setup = self.conn.setup();
        let mapping = self
            .conn
            .get_keyboard_mapping(setup.min_keycode, setup.max_keycode - setup.min_keycode + 1)
            .unwrap()
            .reply()
            .unwrap();
        let per = mapping.keysyms_per_keycode as usize;
        mapping.keysyms[(keycode - setup.min_keycode) as usize * per]
    }

    fn keycode_has_symbols(&self, keycode: u8) -> bool {
        let setup = self.conn.setup();
        let mapping = self
            .conn
            .get_keyboard_mapping(keycode, 1)
            .unwrap()
            .reply()
            .unwrap();
        let _ = setup;
        mapping.keysyms.iter().any(|sym| *sym != 0)
    }
}

fn presses(events: &[Event]) -> Vec<(u8, i16, i16)> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::ButtonPress(press) => Some((press.detail, press.event_x, press.event_y)),
            _ => None,
        })
        .collect()
}

fn key_presses(events: &[Event]) -> Vec<(u8, u16)> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::KeyPress(press) => Some((press.detail, u16::from(press.state))),
            _ => None,
        })
        .collect()
}

#[test]
fn the_primary_area_is_the_whole_screen() {
    let Some(x) = xvfb() else { return };
    let backend = X11Backend::connect(Some(&x.display)).unwrap();
    assert_eq!(
        backend.primary_area().unwrap(),
        Rect {
            x: 0,
            y: 0,
            width: 1024,
            height: 768
        }
    );
    assert!(backend.description().contains(&x.display));
}

#[test]
fn a_capture_shows_a_window_where_it_is() {
    let Some(x) = xvfb() else { return };
    let _window = test_window(&x.display, 100, 100, 200, 100, 0x00ff_0000);
    let backend = X11Backend::connect(Some(&x.display)).unwrap();
    let area = backend.primary_area().unwrap();
    let frame = backend.capture(area).unwrap();
    assert_eq!((frame.width, frame.height), (1024, 768));
    assert_eq!(
        frame.pixel(150, 150),
        [255, 0, 0, 255],
        "inside the red window"
    );
    assert_ne!(frame.pixel(20, 20), [255, 0, 0, 255], "outside it");
}

/// Capturing straight to a smaller screenshot keeps things where they are.
#[test]
fn a_scaled_capture_shows_a_window_where_it_is() {
    let Some(x) = xvfb() else { return };
    let _window = test_window(&x.display, 100, 100, 200, 100, 0x00ff_0000);
    let backend = X11Backend::connect(Some(&x.display)).unwrap();
    let area = backend.primary_area().unwrap();
    let scaling = forge_screen::scale::Scaling::fit(area, 512, 384);
    let frame = backend.capture_scaled(area, &scaling).unwrap();
    assert_eq!((frame.width, frame.height), (512, 384));
    assert_eq!(
        frame.pixel(100, 75),
        [255, 0, 0, 255],
        "inside the red window"
    );
    assert_ne!(frame.pixel(10, 10), [255, 0, 0, 255], "outside it");
}

#[test]
fn clicks_and_double_clicks_reach_the_window_under_the_pointer() {
    let Some(x) = xvfb() else { return };
    let window = test_window(&x.display, 100, 100, 200, 100, 0x0000_00ff);
    let backend = X11Backend::connect(Some(&x.display)).unwrap();

    backend.move_to(Point::new(150, 150)).unwrap();
    assert_eq!(backend.pointer().unwrap(), Point::new(150, 150));
    backend.button(Button::Left, true).unwrap();
    backend.button(Button::Left, false).unwrap();
    backend.button(Button::Right, true).unwrap();
    backend.button(Button::Right, false).unwrap();
    assert_eq!(
        presses(&window.events(Duration::from_millis(300))),
        [(1, 50, 50), (3, 50, 50)]
    );
}

#[test]
fn scrolling_is_wheel_notches() {
    let Some(x) = xvfb() else { return };
    let window = test_window(&x.display, 0, 0, 300, 300, 0);
    let backend = X11Backend::connect(Some(&x.display)).unwrap();
    backend.move_to(Point::new(10, 10)).unwrap();
    backend.scroll(0, 2).unwrap();
    backend.scroll(0, -1).unwrap();
    backend.scroll(1, 0).unwrap();
    let buttons: Vec<u8> = presses(&window.events(Duration::from_millis(300)))
        .into_iter()
        .map(|(button, _, _)| button)
        .collect();
    assert_eq!(buttons, [5, 5, 4, 7]);
}

#[test]
fn chords_hold_their_modifiers() {
    let Some(x) = xvfb() else { return };
    let window = test_window(&x.display, 0, 0, 300, 300, 0);
    window.focus();
    let backend = X11Backend::connect(Some(&x.display)).unwrap();
    backend.key(&KeyChord::parse("ctrl+a").unwrap()).unwrap();
    let pressed = key_presses(&window.events(Duration::from_millis(300)));
    let (keycode, state) = *pressed.last().expect("the key arrived");
    assert_eq!(window.keysym_of(keycode), 'a' as u32);
    assert_ne!(state & 0x4, 0, "Control was held: {state:#x}");
}

#[test]
fn typed_text_arrives_and_the_keymap_is_restored() {
    let Some(x) = xvfb() else { return };
    let window = test_window(&x.display, 0, 0, 300, 300, 0);
    window.focus();
    let backend = X11Backend::connect(Some(&x.display)).unwrap();

    backend.type_text("hi").unwrap();
    let pressed = key_presses(&window.events(Duration::from_millis(300)));
    let typed: Vec<u32> = pressed
        .iter()
        .map(|(keycode, _)| window.keysym_of(*keycode))
        .collect();
    assert_eq!(typed, ['h' as u32, 'i' as u32]);

    // Not on the keymap: typed through a spare keycode, then unmapped.
    backend.type_text("€").unwrap();
    let pressed = key_presses(&window.events(Duration::from_millis(300)));
    assert_eq!(pressed.len(), 1, "one key press for one character");
    assert!(
        !window.keycode_has_symbols(pressed[0].0),
        "the borrowed keycode is given back"
    );
}

/// The guard needs to know who owns a window; the test owns this one.
#[test]
fn a_windows_owner_is_known() {
    let Some(x) = xvfb() else { return };
    let window = test_window(&x.display, 100, 100, 200, 100, 0x00ff_00ff);
    window.focus();
    let backend = X11Backend::connect(Some(&x.display)).unwrap();
    let found = backend
        .window_at(Point::new(150, 150))
        .unwrap()
        .expect("a window is there");
    assert_eq!(found.pid, Some(std::process::id()));
    assert_eq!(found.title, "forge test window");
    let focused = backend
        .focused_window()
        .unwrap()
        .expect("a window has focus");
    assert_eq!(focused.pid, Some(std::process::id()));
    assert!(backend.window_at(Point::new(900, 700)).unwrap().is_none());
    let _ = window
        .conn
        .change_window_attributes(window.window, &ChangeWindowAttributesAux::new());
}

/// The tools on a real display: consent granted, a click lands on the
/// window under it, and a window owned by "Forge" (this process, here) is
/// refused.
#[tokio::test(flavor = "multi_thread")]
async fn the_desktop_tools_drive_a_real_display() {
    use forge_screen::{desktop_tools, Desktop, ScreenController, SCREEN_SCOPE};
    use forge_tools::{ConsentKind, ToolContext};
    use std::sync::Arc;

    let Some(x) = xvfb() else { return };
    let window = test_window(&x.display, 100, 100, 200, 100, 0x00ff_0000);
    let lock = tempfile::tempdir().unwrap();
    let display = x.display.clone();
    let desktop_for = |own_pid: u32| {
        let display = display.clone();
        Desktop::new(
            forge_config::DesktopConfig { enabled: true },
            ScreenController::new(lock.path().join(format!("lock-{own_pid}"))),
            Box::new(move || {
                X11Backend::connect(Some(&display))
                    .map(|backend| Arc::new(backend) as Arc<dyn ScreenBackend>)
            }),
            own_pid,
        )
        .with_settle(std::time::Duration::ZERO)
    };
    let workspace = tempfile::tempdir().unwrap();
    let ctx = || {
        let mut ctx = ToolContext::new(workspace.path().to_path_buf())
            .with_session_id(forge_types::SessionId::new_v4())
            .with_tool_call_id("c1");
        ctx.image_input = true;
        ctx.consents.grant(ConsentKind::ScreenControl, SCREEN_SCOPE);
        ctx
    };

    // Some other process is Forge: the click goes through.
    let tools = desktop_tools(desktop_for(1));
    let click = tools
        .iter()
        .find(|tool| tool.name() == "desktop_click")
        .unwrap();
    let output = click
        .call(&ctx(), serde_json::json!({"x": 150, "y": 150}))
        .await
        .unwrap();
    assert!(!output.is_error, "{}", output.content);
    assert!(
        output.content.contains("forge test window"),
        "{}",
        output.content
    );
    assert_eq!(output.attachments[0].width, Some(1024));
    assert_eq!(
        presses(&window.events(Duration::from_millis(300))),
        [(1, 50, 50)]
    );

    // This process is Forge: its window is off limits.
    let tools = desktop_tools(desktop_for(std::process::id()));
    let click = tools
        .iter()
        .find(|tool| tool.name() == "desktop_click")
        .unwrap();
    let output = click
        .call(&ctx(), serde_json::json!({"x": 150, "y": 150}))
        .await
        .unwrap();
    assert!(output.is_error);
    assert!(
        output.content.contains("Forge's window"),
        "{}",
        output.content
    );
    assert!(presses(&window.events(Duration::from_millis(300))).is_empty());
}

/// Without X-Resource the owner of a window cannot be told, so Forge's own
/// window could not be kept off limits: desktop control is refused.
#[test]
fn a_display_without_x_resource_is_refused() {
    let Some(x) = xvfb_with(&["-extension", "X-Resource"]) else {
        return;
    };
    let error = X11Backend::connect(Some(&x.display))
        .err()
        .expect("refused");
    assert!(error.to_string().contains("X-Resource"), "{error}");
}
