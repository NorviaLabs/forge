//! macOS: not implemented yet. A CoreGraphics backend (CGDisplayCreateImage,
//! CGEventPost, CGWindowListCopyWindowInfo for owner pids) implements the same
//! `ScreenBackend` trait; everything above it is shared.

use crate::backend::Unsupported;

pub fn connect() -> Result<std::sync::Arc<dyn crate::backend::ScreenBackend>, Unsupported> {
    Err(Unsupported::Platform("macOS"))
}
