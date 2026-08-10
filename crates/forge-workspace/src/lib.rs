//! Workspace data services: the filesystem and Git work a frontend needs, with
//! no frontend in it.
//!
//! These modules were already free of any terminal dependency — they read
//! files, run `git status`, and build attachment text — but they lived inside
//! `forge-tui`, so nothing else could use them and nothing stopped a rendering
//! concern from leaking in. Here, the compiler stops it.
//!
//! Glyphs, colours, and anything else describing how this data *looks* stay
//! with the frontend.

pub mod file_context;
pub mod file_ops;
pub mod git_status;
