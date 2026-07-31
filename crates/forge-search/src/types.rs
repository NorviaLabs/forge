use serde::{Deserialize, Serialize};

/// A filename/path fuzzy search hit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileSearchHit {
    pub path: String,
    pub score: i32,
    /// Normalized relevance in `[0.0, 1.0]` relative to the top hit in this result set.
    pub relevance: f32,
}

/// A full-text grep hit with optional surrounding context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrepSearchHit {
    pub path: String,
    pub line: u64,
    pub column: u32,
    pub text: String,
    pub context: Option<String>,
    pub relevance: Option<f32>,
    pub is_definition: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindResponse {
    pub hits: Vec<FileSearchHit>,
    pub total_matched: usize,
    pub total_files: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrepResponse {
    pub hits: Vec<GrepSearchHit>,
    pub total_matched: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrepQueryMode {
    Plain,
    Regex,
    Fuzzy,
}
