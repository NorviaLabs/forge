//! Workspace indexing and fast file/content search for Forge agents and UI.

mod index;
mod quick_open;
mod types;

pub use index::{SearchError, WorkspaceIndex, WorkspaceIndexOptions};
pub use quick_open::score_quick_open;
pub use types::{FileSearchHit, FindResponse, GrepQueryMode, GrepResponse, GrepSearchHit};
