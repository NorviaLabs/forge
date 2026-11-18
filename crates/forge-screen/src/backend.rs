//! The platform boundary: what desktop control needs from a display.
//!
//! Everything above this trait (consent, the screen lease, takeover, the
//! Forge-window guard, scaling, the tools) is platform-neutral. Linux X11
//! implements it today; macOS is a second implementation of the same trait.

use thiserror::Error;

use crate::keys::KeyChord;
use crate::scale::{downscale, Scaling};

/// A point in screen pixels (root-window coordinates).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// A rectangle in screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn contains(&self, point: Point) -> bool {
        i64::from(point.x) >= i64::from(self.x)
            && i64::from(point.y) >= i64::from(self.y)
            && i64::from(point.x) < i64::from(self.x) + i64::from(self.width)
            && i64::from(point.y) < i64::from(self.y) + i64::from(self.height)
    }
}

/// Captured pixels, tightly packed RGBA, row-major.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Frame {
    /// The RGBA of pixel `(x, y)`.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * self.width + x) * 4) as usize;
        [
            self.rgba[at],
            self.rgba[at + 1],
            self.rgba[at + 2],
            self.rgba[at + 3],
        ]
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    Left,
    Middle,
    Right,
}

/// A top-level window, as far as the guard needs to know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// The process that owns the window, when the display can tell.
    pub pid: Option<u32>,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BackendError {
    #[error("display error: {0}")]
    Display(String),
    #[error("cannot type {0:?}: no free key to map it to")]
    Untypable(char),
}

/// What desktop control needs from the display.
pub trait ScreenBackend: Send + Sync {
    /// "X11 :0" and the like, for messages.
    fn description(&self) -> String;
    /// The primary display's area.
    fn primary_area(&self) -> Result<Rect, BackendError>;
    fn capture(&self, area: Rect) -> Result<Frame, BackendError>;
    /// Capture `scaling.area` scaled to the screenshot. Backends that can
    /// scale straight from what the display sends override this to skip the
    /// full-size RGBA copy.
    fn capture_scaled(&self, area: Rect, scaling: &Scaling) -> Result<Frame, BackendError> {
        Ok(downscale(&self.capture(area)?, scaling))
    }
    fn pointer(&self) -> Result<Point, BackendError>;
    fn move_to(&self, point: Point) -> Result<(), BackendError>;
    fn button(&self, button: Button, pressed: bool) -> Result<(), BackendError>;
    /// Scroll by whole notches: positive `dy` is down, positive `dx` right.
    fn scroll(&self, dx: i32, dy: i32) -> Result<(), BackendError>;
    fn key(&self, chord: &KeyChord) -> Result<(), BackendError>;
    fn type_text(&self, text: &str) -> Result<(), BackendError>;
    /// The top-level window under `point`.
    fn window_at(&self, point: Point) -> Result<Option<WindowInfo>, BackendError>;
    /// The window with keyboard focus.
    fn focused_window(&self) -> Result<Option<WindowInfo>, BackendError>;
}

/// Why desktop control cannot run here.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Unsupported {
    #[error("Needs an X11 session. This is a Wayland session, where other apps' windows cannot be seen or controlled.")]
    Wayland,
    #[error("No display: DISPLAY is not set.")]
    NoDisplay,
    #[error("Desktop control is not available on {0} yet.")]
    Platform(&'static str),
    #[error("Could not use the display: {0}")]
    Connect(String),
}

/// Decide whether this session can be controlled, from its environment,
/// before connecting. `env` reads a variable.
pub fn session_support(env: &dyn Fn(&str) -> Option<String>) -> Result<(), Unsupported> {
    if cfg!(target_os = "linux") {
        let set = |name: &str| env(name).is_some_and(|value| !value.is_empty());
        let wayland_session = env("XDG_SESSION_TYPE").is_some_and(|kind| kind == "wayland");
        if wayland_session || (set("WAYLAND_DISPLAY") && !set("DISPLAY")) {
            return Err(Unsupported::Wayland);
        }
        if !set("DISPLAY") {
            return Err(Unsupported::NoDisplay);
        }
        Ok(())
    } else if cfg!(target_os = "macos") {
        Err(Unsupported::Platform("macOS"))
    } else {
        Err(Unsupported::Platform(std::env::consts::OS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::collections::HashMap;

    #[cfg(target_os = "linux")]
    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn rects_contain_their_own_pixels_only() {
        let rect = Rect {
            x: 100,
            y: 50,
            width: 10,
            height: 5,
        };
        assert!(rect.contains(Point::new(100, 50)));
        assert!(rect.contains(Point::new(109, 54)));
        assert!(!rect.contains(Point::new(110, 54)));
        assert!(!rect.contains(Point::new(99, 50)));
    }

    #[test]
    fn pixels_are_read_row_major() {
        let frame = Frame {
            width: 2,
            height: 2,
            rgba: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
        };
        assert_eq!(frame.pixel(1, 0), [5, 6, 7, 8]);
        assert_eq!(frame.pixel(0, 1), [9, 10, 11, 12]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_needs_an_x11_display() {
        assert_eq!(session_support(&env(&[("DISPLAY", ":0")])), Ok(()));
        assert_eq!(
            session_support(&env(&[("DISPLAY", ":0"), ("XDG_SESSION_TYPE", "x11")])),
            Ok(())
        );
        assert_eq!(session_support(&env(&[])), Err(Unsupported::NoDisplay));
        assert_eq!(
            session_support(&env(&[("WAYLAND_DISPLAY", "wayland-0")])),
            Err(Unsupported::Wayland)
        );
        // XWayland: a DISPLAY exists, but only X clients could be seen.
        assert_eq!(
            session_support(&env(&[
                ("DISPLAY", ":0"),
                ("WAYLAND_DISPLAY", "wayland-0"),
                ("XDG_SESSION_TYPE", "wayland"),
            ])),
            Err(Unsupported::Wayland)
        );
    }

    #[test]
    fn unsupported_reasons_read_as_settings_notes() {
        assert!(Unsupported::Wayland
            .to_string()
            .starts_with("Needs an X11 session."));
        assert!(Unsupported::Platform("macOS").to_string().contains("macOS"));
    }
}
