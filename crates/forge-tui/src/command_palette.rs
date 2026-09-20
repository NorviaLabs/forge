//! Slash-command palette data and filtering.

#[derive(Debug, Clone)]
pub struct PaletteItem {
    pub cmd: String,
    pub desc: String,
    pub is_skill: bool,
}

impl PaletteItem {
    pub fn display_cmd(&self) -> String {
        if self.is_skill {
            format!("/skill:{}", self.cmd.trim_start_matches('/'))
        } else {
            self.cmd.clone()
        }
    }
}

pub fn default_palette_items() -> Vec<PaletteItem> {
    vec![
        ("/help", "Show help and keyboard shortcuts"),
        (
            "/connect",
            "Connect provider (xAI, OpenCode Go/Zen, OpenAI, Anthropic, Ollama)",
        ),
        ("/model", "Switch model for future turns"),
        ("/theme", "Switch presentation theme"),
        ("/status", "Show session status and diagnostics"),
        ("/context", "Show the token budget broken down by category"),
        ("/plan", "Inspect the latest execution plan"),
        ("/effort", "Set model effort"),
        ("/thinking", "Toggle model thinking"),
        (
            "/approve-all",
            "Toggle approve-all for this session (disables sandbox; asks first)",
        ),
        ("/compact", "Continue in a fresh context"),
        ("/resume", "Restore a previous session"),
        ("/continue", "Resume the most recent previous session"),
        ("/new", "Start a new session"),
        ("/fork", "Start a new session from this conversation"),
        (
            "/sessions",
            "Switch, create, attach, and manage repository sessions",
        ),
        ("/terminal", "Open the terminal panel (Ctrl+`)"),
        ("/clear", "Clear the TUI screen"),
        ("/edit", "Open a workspace file in the embedded editor"),
        (
            "/context-file",
            "Attach the active file to the next message",
        ),
        ("/disconnect", "Log out and clear credentials"),
        (
            "/quit",
            "Close the current session; on the primary session, quit Forge",
        ),
    ]
    .into_iter()
    .map(|(cmd, desc)| PaletteItem {
        cmd: cmd.into(),
        desc: desc.into(),
        is_skill: false,
    })
    .collect()
}

pub fn filter_palette(filter: &str) -> Vec<PaletteItem> {
    let f = filter.trim().trim_start_matches('/').to_ascii_lowercase();
    let mut items = default_palette_items()
        .into_iter()
        .filter(|item| {
            f.is_empty()
                || item.cmd.to_ascii_lowercase().contains(&f)
                || item.desc.to_ascii_lowercase().contains(&f)
        })
        .collect::<Vec<_>>();
    items.sort_by_key(|item| {
        let command = item.cmd.trim_start_matches('/').to_ascii_lowercase();
        if command.starts_with(&f) {
            0
        } else if command.contains(&f) {
            1
        } else {
            2
        }
    });
    items
}
