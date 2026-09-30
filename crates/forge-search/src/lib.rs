//! Workspace indexing and fast file/content search for Forge agents and UI.

mod index;
mod quick_open;
mod types;

pub use index::{
    merge_search_results, SearchError, WorkspaceIndex, WorkspaceIndexOptions,
    DEFAULT_SEARCH_MAX_FILES, MIN_CONTENT_QUERY_CHARS,
};
pub use quick_open::score_quick_open;
pub use types::{
    FileSearchHit, FindResponse, GrepQueryMode, GrepResponse, GrepSearchHit, MergedSearch,
};
