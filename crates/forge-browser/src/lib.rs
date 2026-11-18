//! Browser control: drive an installed Chromium-family browser page by page.
//!
//! The agent works through a fresh-profile browser that Forge starts on
//! first use and talks to over the DevTools pipe. It reads a page as a list
//! of interactive elements with refs, and acts with real input events:
//! clicks, double-clicks, typing, keys, scrolling, selecting, submitting.
//! All of the browser's traffic goes through the session's egress gateway.
//! Submitting a form needs the user's consent; typing into a password field
//! is refused.

pub mod cdp;
pub mod checker;
pub mod deny_proxy;
pub mod discovery;
pub mod keys;
pub mod launch;
pub mod manager;
mod page;
pub mod snapshot;
mod tools;
pub mod web_check;

pub use discovery::{discover, discover_all, DiscoveryError, FoundBrowser};
pub use manager::{BrowserManager, BrowserStatus, ManagerOptions};
pub use tools::{browser_tools, BROWSER_TOOL_NAMES};
pub use web_check::{web_tools, WebCheckManager, WebCheckOptions, WEB_TOOL_NAMES};
