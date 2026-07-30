//! Shared path and footer helpers for the `app` module.
//!
//! Split out of `app.rs` per #19. Small free functions used across several
//! `TuiApp` seams. Moved verbatim.

use std::path::{Path, PathBuf};

#[allow(dead_code)]
pub(crate) fn footer_provider_id(provider: &str, connect_profile: Option<&str>) -> String {
    connect_profile.unwrap_or(provider).to_owned()
}

pub(crate) fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

pub(crate) fn rebase_path(path: &Path, old_base: &Path, new_base: &Path) -> Option<PathBuf> {
    if path == old_base {
        return Some(new_base.to_path_buf());
    }
    path.strip_prefix(old_base)
        .ok()
        .map(|suffix| new_base.join(suffix))
}
