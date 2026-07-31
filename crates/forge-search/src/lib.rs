//! Workspace indexing and fast file/content search for Forge agents and UI.

mod index;
mod types;

pub use index::{SearchError, WorkspaceIndex, WorkspaceIndexOptions};
pub use types::{FileSearchHit, FindResponse, GrepQueryMode, GrepResponse, GrepSearchHit};
