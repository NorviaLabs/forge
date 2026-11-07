//! Forge desktop: the walking skeleton (phase K1).
//!
//! Runs the repository supervisor in-process and bridges it to the webview
//! over Tauri IPC: one `Channel` of events per window, a small command
//! allowlist, no network listener. Startup goes through the same UI-free
//! `forge_session::LaunchPlan` the TUI uses.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod commands_list;
mod github;
mod pinned;
mod providers;
#[cfg(feature = "self-test")]
mod self_test;
mod shell;
mod startup;
mod themes;
mod workspace;

fn main() {
    std::process::exit(startup::run());
}
