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
mod last_turn;
mod pinned;
mod providers;
#[cfg(feature = "self-test")]
mod self_test;
mod shell;
mod startup;
mod themes;
mod workspace;

fn main() {
    // Git's SSH routing runs `<this executable> __egress-connect ...` as its
    // ProxyCommand. Handle it before startup parses arguments, as `forge`
    // does; otherwise every SSH push or fetch from a desktop session fails.
    let mut args = std::env::args_os().skip(1);
    if args
        .next()
        .is_some_and(|arg| arg == forge_tools::egress_connect::SUBCOMMAND)
    {
        let rest: Vec<String> = args.map(|arg| arg.to_string_lossy().into_owned()).collect();
        std::process::exit(forge_tools::egress_connect::run(&rest));
    }
    if let Ok(filter) = tracing_subscriber::EnvFilter::try_from_env("FORGE_LOG") {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .init();
    }
    std::process::exit(startup::run());
}
