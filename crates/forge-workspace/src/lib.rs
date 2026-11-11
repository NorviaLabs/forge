//! Workspace data services: the filesystem and Git work a frontend needs, with
//! no frontend in it.
//!
//! These modules read files, run `git status`, and build attachment text. They
//! hold no rendering concern, and keeping them out of the frontend crate lets
//! the compiler keep it that way.
//!
//! Glyphs, colours, and anything else describing how this data *looks* stay
//! with the frontend.

pub mod file_context;
pub mod gh;
pub mod git_review;
pub mod git_service;
pub mod git_status;
pub mod github;
