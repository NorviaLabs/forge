//! Desktop control: see the user's screen and drive the real mouse and
//! keyboard, with the user's consent, one session at a time.
//!
//! Platform code sits behind [`backend::ScreenBackend`]; Linux X11 is
//! implemented, macOS is a stub returning [`backend::Unsupported`].

pub mod backend;
pub mod keys;
pub mod lease;
pub mod scale;
pub mod takeover;
mod tools;

#[cfg(target_os = "linux")]
pub mod linux_x11;
#[cfg(target_os = "macos")]
mod macos;

use std::sync::Arc;

pub use backend::{ScreenBackend, Unsupported};
pub use lease::ScreenController;
pub use tools::{desktop_tools, Connector, Desktop, DESKTOP_TOOL_NAMES, SCREEN_SCOPE};

/// Whether this machine's session can be controlled, from its environment
/// alone (no connection is made).
pub fn session_support() -> Result<(), Unsupported> {
    backend::session_support(&|name| std::env::var(name).ok())
}

/// Connect to this machine's display.
pub fn connect() -> Result<Arc<dyn ScreenBackend>, Unsupported> {
    session_support()?;
    #[cfg(target_os = "linux")]
    {
        Ok(Arc::new(linux_x11::X11Backend::connect(None)?))
    }
    #[cfg(target_os = "macos")]
    {
        macos::connect()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(Unsupported::Platform(std::env::consts::OS))
    }
}
