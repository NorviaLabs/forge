// The exact IPC command allowlist, shared by `build.rs` (which generates the
// per-command permissions Tauri enforces) and the invoke handler. A command
// missing here is not callable from the webview at all.

/// Commands every build exposes.
#[allow(dead_code)] // read by build.rs and the config tests
pub const DESKTOP_COMMANDS: &[&str] = &[
    "subscribe",
    "submit_prompt",
    "stop_turn",
    "approve_once",
    "deny",
];
