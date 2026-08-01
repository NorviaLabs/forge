//! Integration tests for [`TuiApp`].
//!
//! Split across `app/tests/*.rs` per #19. Shared fixtures live in [`helpers`].

mod activity;
mod approval;
mod characterization;
mod chrome;
mod commands;
mod connect;
mod conversation_cache;
mod edge;
mod editor;
mod explorer;
mod focus;
pub(crate) mod helpers;
mod highlight;
mod pointer;
mod prelude;
mod run;
mod tasks;
mod theme;
mod watch;
mod workspace;
