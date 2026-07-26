//! Overlays: HITL, slash palette, model picker (TUI-04).

use crate::theme;
use forge_types::HitlPayload;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Widget};
use std::path::Path;

#[derive(Debug, Clone)]
pub enum Overlay {
    Welcome,
    Hitl {
        payload: HitlPayload,
    },
    TurnLimit {
        turns: u32,
    },
    Slash {
        filter: String,
        selected: usize,
        items: Vec<PaletteItem>,
    },
    Model {
        provider_selected: usize,
        model_selected: usize,
        model_input: String,
        /// Provider ids (connect profile ids) derived from `items`.
        providers: Vec<String>,
        items: Vec<ModelItem>,
    },
    /// Phase 6.1 — OpenCode Go (and other ApiKey tui_always_prompt profiles)
    ConnectApiKey {
        profile_id: String,
        title: String,
        auth_url: Option<String>,
        /// Masked key buffer (stored plain in memory only for submit).
        key_input: String,
        /// Optional hint when env key exists
        env_hint: Option<String>,
        /// Validation/authentication error shown inside the modal.
        error: Option<String>,
    },
    /// Phase 6.1 — xAI Grok OAuth progress
    ConnectOauth {
        profile_id: String,
        title: String,
        instructions: String,
    },
    /// Phase 6/8 — pick a connect profile after `/connect`
    ConnectPicker {
        selected: usize,
        items: Vec<ConnectProfileItem>,
    },
    ResumePicker {
        selected: usize,
        items: Vec<ResumeSessionItem>,
    },
    FileExplorer {
        cwd: String,
        selected: usize,
        items: Vec<FileExplorerItem>,
        error: Option<String>,
    },
    FileViewer {
        path: String,
        lines: Vec<String>,
        scroll: usize,
    },
}

#[derive(Debug, Clone)]
pub struct FileExplorerItem {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone)]
pub struct ConnectProfileItem {
    pub id: String,
    pub title: String,
    pub auth_mode: String,
    pub auth_url: Option<String>,
    pub connected: bool,
}

#[derive(Debug, Clone)]
pub struct ResumeSessionItem {
    pub id: String,
    pub modified: String,
}

#[derive(Debug, Clone)]
pub struct PaletteItem {
    pub cmd: String,
    pub desc: String,
}

#[derive(Debug, Clone)]
pub struct ModelItem {
    pub provider: String,
    pub model: String,
    /// Optional connect profile that sourced this row (catalog).
    pub profile_id: Option<String>,
}

pub fn default_palette_items() -> Vec<PaletteItem> {
    // Keep in sync with `commands::parse_slash`.
    vec![
        PaletteItem {
            cmd: "/status".into(),
            desc: "Session, budget, journal cursor".into(),
        },
        PaletteItem {
            cmd: "/cost".into(),
            desc: "Provider limits, usage, and balance".into(),
        },
        PaletteItem {
            cmd: "/connect".into(),
            desc: "Connect provider (xAI, OpenCode Go/Zen, OpenAI, Anthropic, Ollama)".into(),
        },
        PaletteItem {
            cmd: "/model".into(),
            desc: "Switch provider/model (config only)".into(),
        },
        PaletteItem {
            cmd: "/model refresh".into(),
            desc: "Refresh model catalogs".into(),
        },
        PaletteItem {
            cmd: "/effort".into(),
            desc: "Set model reasoning effort".into(),
        },
        PaletteItem {
            cmd: "/compact".into(),
            desc: "Force handoff + clear context".into(),
        },
        PaletteItem {
            cmd: "/resume".into(),
            desc: "Resume session by id".into(),
        },
        PaletteItem {
            cmd: "/file".into(),
            desc: "Browse and read one file (readonly)".into(),
        },
        PaletteItem {
            cmd: "/sync".into(),
            desc: "Commit + push (message from changeset)".into(),
        },
        PaletteItem {
            cmd: "/copy".into(),
            desc: "Copy last answer".into(),
        },
        PaletteItem {
            cmd: "/clear".into(),
            desc: "Clear the TUI screen".into(),
        },
        PaletteItem {
            cmd: "/disconnect".into(),
            desc: "Log out and clear credentials".into(),
        },
        PaletteItem {
            cmd: "/quit".into(),
            desc: "Exit TUI".into(),
        },
    ]
}

/// Model list from cached provider catalogs (no network).
pub fn default_models() -> Vec<ModelItem> {
    let mut items = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for p in forge_connect::builtin_registry().profiles() {
        for m in &p.default_models {
            if seen.insert(m.clone()) {
                items.push(ModelItem {
                    provider: "native".into(),
                    model: m.clone(),
                    profile_id: Some(p.id.clone()),
                });
            }
        }
    }
    items
}

/// Build picker rows from live or cached catalog entries.
pub fn models_from_catalog(entries: &[forge_connect::CatalogEntry]) -> Vec<ModelItem> {
    entries
        .iter()
        .map(|e| ModelItem {
            provider: "native".into(),
            model: e.id.clone(),
            profile_id: Some(e.profile_id.clone()),
        })
        .collect()
}

impl Overlay {
    pub fn welcome() -> Self {
        Self::Welcome
    }

    pub fn slash_open(filter: impl Into<String>) -> Self {
        let filter = filter.into();
        let items = filter_palette(&filter);
        Self::Slash {
            filter,
            selected: 0,
            items,
        }
    }

    pub fn model_open() -> Self {
        Self::model_open_with(default_models())
    }

    pub fn model_open_with(items: Vec<ModelItem>) -> Self {
        let mut providers: Vec<String> = items
            .iter()
            .filter_map(|m| {
                if let Some(ref pid) = m.profile_id {
                    Some(pid.clone())
                } else {
                    let pfx = m.model.split('/').next().unwrap_or("").trim();
                    if pfx.is_empty() {
                        None
                    } else {
                        Some(pfx.to_string())
                    }
                }
            })
            .collect();
        providers.sort();
        providers.dedup();
        if providers.is_empty() {
            providers.push("all".into());
        }
        Self::Model {
            provider_selected: 0,
            model_selected: 0,
            model_input: String::new(),
            providers,
            items,
        }
    }

    /// Focus the model picker on the given model id (best-effort).
    pub fn focus_model(&mut self, model_id: &str) {
        let needle = model_id.trim();
        if needle.is_empty() {
            return;
        }
        let Self::Model {
            provider_selected,
            model_selected,
            providers,
            items,
            ..
        } = self
        else {
            return;
        };

        // Find exact model first.
        if let Some(found) = items.iter().find(|m| m.model == needle) {
            let pid = found
                .profile_id
                .clone()
                .unwrap_or_else(|| needle.split('/').next().unwrap_or("").to_string());
            if let Some(pi) = providers.iter().position(|p| p == &pid) {
                *provider_selected = pi;
            }
            let active_pid = providers
                .get(*provider_selected)
                .map(|s| s.as_str())
                .unwrap_or("all");
            if let Some(mi) = items
                .iter()
                .filter(|m| model_matches_provider(active_pid, m))
                .position(|m| m.model == needle)
            {
                *model_selected = mi;
            }
            return;
        }

        // Fallback: focus provider by prefix.
        let prefix = needle.split('/').next().unwrap_or("").trim();
        if prefix.is_empty() {
            return;
        }
        if let Some(pi) = providers.iter().position(|p| p == prefix) {
            *provider_selected = pi;
            *model_selected = 0;
        }
    }

    pub fn hitl(payload: HitlPayload) -> Self {
        Self::Hitl { payload }
    }

    pub fn turn_limit(turns: u32) -> Self {
        Self::TurnLimit { turns }
    }

    pub fn connect_api_key(
        profile_id: impl Into<String>,
        title: impl Into<String>,
        auth_url: Option<String>,
        env_hint: Option<String>,
    ) -> Self {
        Self::ConnectApiKey {
            profile_id: profile_id.into(),
            title: title.into(),
            auth_url,
            key_input: String::new(),
            env_hint,
            error: None,
        }
    }

    pub fn connect_oauth(
        profile_id: impl Into<String>,
        title: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Self {
        Self::ConnectOauth {
            profile_id: profile_id.into(),
            title: title.into(),
            instructions: instructions.into(),
        }
    }

    pub fn connect_picker(items: Vec<ConnectProfileItem>) -> Self {
        let mut items = items;
        items.sort_by(|a, b| {
            a.title
                .to_ascii_lowercase()
                .cmp(&b.title.to_ascii_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        Self::ConnectPicker { selected: 0, items }
    }

    pub fn resume_picker(items: Vec<ResumeSessionItem>) -> Self {
        Self::ResumePicker { selected: 0, items }
    }

    pub fn file_explorer(
        cwd: impl Into<String>,
        items: Vec<FileExplorerItem>,
        error: Option<String>,
    ) -> Self {
        Self::FileExplorer {
            cwd: cwd.into(),
            selected: 0,
            items,
            error,
        }
    }

    pub fn file_viewer(path: impl Into<String>, contents: impl AsRef<str>) -> Self {
        Self::FileViewer {
            path: path.into(),
            lines: contents.as_ref().lines().map(str::to_string).collect(),
            scroll: 0,
        }
    }

    pub fn filter_slash(&mut self, f: &str) {
        if let Self::Slash {
            filter,
            selected,
            items,
        } = self
        {
            *filter = f.to_string();
            *items = filter_palette(f);
            *selected = 0;
        }
    }

    pub fn move_sel(&mut self, delta: i32) {
        match self {
            Self::Slash {
                selected, items, ..
            } => {
                if items.is_empty() {
                    return;
                }
                let n = items.len() as i32;
                *selected = ((*selected as i32 + delta).rem_euclid(n)) as usize;
            }
            Self::Model {
                provider_selected,
                model_selected,
                model_input,
                providers,
                items,
            } => {
                if items.is_empty() {
                    return;
                }
                let pid = providers
                    .get(*provider_selected)
                    .map(|s| s.as_str())
                    .unwrap_or("all");
                let n = filtered_models_len(pid, model_input, items).max(1) as i32;
                *model_selected = ((*model_selected as i32 + delta).rem_euclid(n)) as usize;
            }
            Self::ConnectPicker {
                selected, items, ..
            } => {
                if items.is_empty() {
                    return;
                }
                let n = items.len() as i32;
                *selected = ((*selected as i32 + delta).rem_euclid(n)) as usize;
            }
            Self::ResumePicker {
                selected, items, ..
            } => {
                if items.is_empty() {
                    return;
                }
                let n = items.len() as i32;
                *selected = ((*selected as i32 + delta).rem_euclid(n)) as usize;
            }
            Self::FileExplorer {
                selected, items, ..
            } => {
                if items.is_empty() {
                    return;
                }
                let n = items.len() as i32;
                *selected = ((*selected as i32 + delta).rem_euclid(n)) as usize;
            }
            Self::FileViewer { scroll, lines, .. } => {
                if delta < 0 {
                    *scroll = scroll.saturating_sub(delta.unsigned_abs() as usize);
                } else {
                    *scroll = (*scroll + delta as usize).min(lines.len().saturating_sub(1));
                }
            }
            _ => {}
        }
    }
}

fn model_matches_provider(provider_id: &str, m: &ModelItem) -> bool {
    if provider_id == "all" {
        return true;
    }
    if let Some(ref pid) = m.profile_id {
        if pid == provider_id {
            return true;
        }
    }
    let pfx = m.model.split('/').next().unwrap_or("").trim();
    pfx == provider_id
}

fn model_matches_input(model_input: &str, item: &ModelItem) -> bool {
    let needle = model_input.trim().to_ascii_lowercase();
    needle.is_empty() || item.model.to_ascii_lowercase().contains(&needle)
}

fn filtered_models_len(provider_id: &str, model_input: &str, items: &[ModelItem]) -> usize {
    items
        .iter()
        .filter(|m| model_matches_provider(provider_id, m) && model_matches_input(model_input, m))
        .count()
}

pub fn filter_palette(filter: &str) -> Vec<PaletteItem> {
    let f = filter.trim().trim_start_matches('/').to_ascii_lowercase();
    let mut items = default_palette_items()
        .into_iter()
        .filter(|i| {
            if f.is_empty() {
                return true;
            }
            i.cmd.to_ascii_lowercase().contains(&f) || i.desc.to_ascii_lowercase().contains(&f)
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

/// Result of handling a key inside an overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayAction {
    None,
    Close,
    BeginOnboarding,
    /// Close HITL and approve/deny
    HitlApprove,
    /// Approve and allow this tool for the rest of the session
    HitlApproveSession,
    HitlDeny,
    ContinueTurns,
    StopTurns,
    /// Execute slash command string e.g. "/status"
    RunCommand(String),
    /// Insert into input
    InsertInput(String),
    /// Model selection
    SelectModel {
        provider: String,
        model: String,
    },
    /// Submit API key from ConnectApiKey overlay
    ConnectSubmitKey {
        profile_id: String,
        api_key: String,
    },
    /// Poll / continue OAuth from ConnectOauth overlay (Enter)
    ConnectCompleteOauth {
        profile_id: String,
    },
    /// Use env key without typing (secondary action on API key modal)
    ConnectUseEnv {
        profile_id: String,
    },
    /// User picked a connect profile from the picker
    ConnectPickProfile {
        profile_id: String,
    },
    FilePick {
        path: String,
        is_dir: bool,
    },
}

pub fn handle_overlay_key(overlay: &mut Overlay, key: Key) -> OverlayAction {
    match key {
        Key::Esc if matches!(overlay, Overlay::TurnLimit { .. }) => OverlayAction::StopTurns,
        Key::Esc => {
            // HITL: Esc dismisses focus but does not approve/deny (design recommendation)
            if matches!(overlay, Overlay::Hitl { .. }) {
                return OverlayAction::Close;
            }
            OverlayAction::Close
        }
        Key::Up => {
            overlay.move_sel(-1);
            OverlayAction::None
        }
        Key::Down => {
            overlay.move_sel(1);
            OverlayAction::None
        }
        Key::Left => {
            if let Some(path) = match overlay {
                Overlay::FileExplorer { cwd, .. } => parent_dir(cwd),
                Overlay::FileViewer { path, .. } => parent_dir(path),
                _ => None,
            } {
                return OverlayAction::FilePick { path, is_dir: true };
            }
            if let Overlay::Model {
                provider_selected,
                model_selected,
                providers,
                ..
            } = overlay
            {
                if !providers.is_empty() {
                    let n = providers.len() as i32;
                    *provider_selected = ((*provider_selected as i32 - 1).rem_euclid(n)) as usize;
                    *model_selected = 0;
                }
            }
            OverlayAction::None
        }
        Key::Right => {
            if let Overlay::Model {
                provider_selected,
                model_selected,
                providers,
                ..
            } = overlay
            {
                if !providers.is_empty() {
                    let n = providers.len() as i32;
                    *provider_selected = ((*provider_selected as i32 + 1).rem_euclid(n)) as usize;
                    *model_selected = 0;
                }
            }
            OverlayAction::None
        }
        Key::Enter => match overlay {
            Overlay::Welcome => OverlayAction::BeginOnboarding,
            Overlay::TurnLimit { .. } => OverlayAction::ContinueTurns,
            Overlay::Slash {
                selected, items, ..
            } => {
                if let Some(item) = items.get(*selected) {
                    let cmd = item.cmd.clone();
                    // no-arg commands execute; others insert
                    if matches!(
                        cmd.as_str(),
                        "/status"
                            | "/model refresh"
                            | "/quit"
                            | "/compact"
                            | "/sync"
                            | "/copy"
                            | "/clear"
                            | "/file"
                    ) {
                        OverlayAction::RunCommand(cmd)
                    } else {
                        OverlayAction::InsertInput(format!("{cmd} "))
                    }
                } else {
                    OverlayAction::None
                }
            }
            Overlay::Model {
                provider_selected,
                model_selected,
                model_input,
                providers,
                items,
            } => {
                let pid = providers
                    .get(*provider_selected)
                    .map(|s| s.as_str())
                    .unwrap_or("all");
                let chosen = items
                    .iter()
                    .filter(|m| {
                        model_matches_provider(pid, m) && model_matches_input(model_input, m)
                    })
                    .nth(*model_selected);
                if !model_input.trim().is_empty()
                    && !chosen.is_some_and(|m| m.model.eq_ignore_ascii_case(model_input.trim()))
                {
                    OverlayAction::RunCommand(format!("/model {}", model_input.trim()))
                } else if let Some(m) = chosen {
                    OverlayAction::SelectModel {
                        provider: m.provider.clone(),
                        model: m.model.clone(),
                    }
                } else {
                    OverlayAction::None
                }
            }
            Overlay::ConnectApiKey {
                profile_id,
                key_input,
                ..
            } => {
                if key_input.trim().is_empty() {
                    OverlayAction::None
                } else {
                    OverlayAction::ConnectSubmitKey {
                        profile_id: profile_id.clone(),
                        api_key: key_input.clone(),
                    }
                }
            }
            Overlay::ConnectOauth { profile_id, .. } => OverlayAction::ConnectCompleteOauth {
                profile_id: profile_id.clone(),
            },
            Overlay::ConnectPicker {
                selected, items, ..
            } => {
                if let Some(it) = items.get(*selected) {
                    OverlayAction::ConnectPickProfile {
                        profile_id: it.id.clone(),
                    }
                } else {
                    OverlayAction::None
                }
            }
            Overlay::ResumePicker {
                selected, items, ..
            } => {
                if let Some(item) = items.get(*selected) {
                    OverlayAction::RunCommand(format!("/resume {}", item.id))
                } else {
                    OverlayAction::None
                }
            }
            Overlay::FileExplorer {
                selected, items, ..
            } => {
                if let Some(item) = items.get(*selected) {
                    OverlayAction::FilePick {
                        path: item.path.clone(),
                        is_dir: item.is_dir,
                    }
                } else {
                    OverlayAction::None
                }
            }
            Overlay::FileViewer { .. } => OverlayAction::None,
            Overlay::Hitl { .. } => OverlayAction::None,
        },
        // Use-env must NOT steal literal e/E from pasted API keys (keys almost always
        // contain those letters). Only when the field is still empty + env is available.
        Key::Char('e') | Key::Char('E')
            if matches!(
                overlay,
                Overlay::ConnectApiKey {
                    env_hint: Some(_),
                    key_input,
                    ..
                } if key_input.is_empty()
            ) =>
        {
            if let Overlay::ConnectApiKey { profile_id, .. } = overlay {
                OverlayAction::ConnectUseEnv {
                    profile_id: profile_id.clone(),
                }
            } else {
                OverlayAction::None
            }
        }
        Key::Char(c) if matches!(overlay, Overlay::ConnectApiKey { .. }) => {
            if let Overlay::ConnectApiKey { key_input, .. } = overlay {
                // Ignore whitespace/newlines from sloppy paste; keep other printable chars.
                if !c.is_control() && !c.is_whitespace() {
                    key_input.push(c);
                }
            }
            OverlayAction::None
        }
        Key::Paste(ref data) if matches!(overlay, Overlay::ConnectApiKey { .. }) => {
            if let Overlay::ConnectApiKey { key_input, .. } = overlay {
                for c in data.chars() {
                    if !c.is_control() && !c.is_whitespace() {
                        key_input.push(c);
                    }
                }
            }
            OverlayAction::None
        }
        Key::Char(c) if matches!(overlay, Overlay::Model { .. }) => {
            if let Overlay::Model {
                model_input,
                model_selected,
                ..
            } = overlay
            {
                if !c.is_control() && !c.is_whitespace() {
                    model_input.push(c);
                    *model_selected = 0;
                }
            }
            OverlayAction::None
        }
        Key::Paste(ref data) if matches!(overlay, Overlay::Model { .. }) => {
            if let Overlay::Model {
                model_input,
                model_selected,
                ..
            } = overlay
            {
                for c in data.chars() {
                    if !c.is_control() && !c.is_whitespace() {
                        model_input.push(c);
                    }
                }
                *model_selected = 0;
            }
            OverlayAction::None
        }
        Key::Backspace if matches!(overlay, Overlay::Model { .. }) => {
            if let Overlay::Model {
                model_input,
                model_selected,
                ..
            } = overlay
            {
                model_input.pop();
                *model_selected = 0;
            }
            OverlayAction::None
        }
        Key::Backspace if matches!(overlay, Overlay::ConnectApiKey { .. }) => {
            if let Overlay::ConnectApiKey { key_input, .. } = overlay {
                key_input.pop();
            }
            OverlayAction::None
        }
        Key::Backspace
            if matches!(
                overlay,
                Overlay::FileExplorer { .. } | Overlay::FileViewer { .. }
            ) =>
        {
            match overlay {
                Overlay::FileExplorer { cwd, .. } => parent_dir(cwd),
                Overlay::FileViewer { path, .. } => parent_dir(path),
                _ => None,
            }
            .map(|path| OverlayAction::FilePick { path, is_dir: true })
            .unwrap_or(OverlayAction::None)
        }
        Key::Char('a') | Key::Char('A') if matches!(overlay, Overlay::Hitl { .. }) => {
            OverlayAction::HitlApprove
        }
        Key::Char('s') | Key::Char('S') if matches!(overlay, Overlay::Hitl { .. }) => {
            OverlayAction::HitlApproveSession
        }
        Key::Char('d') | Key::Char('D') if matches!(overlay, Overlay::Hitl { .. }) => {
            OverlayAction::HitlDeny
        }
        Key::Char('y') | Key::Char('Y') if matches!(overlay, Overlay::TurnLimit { .. }) => {
            OverlayAction::ContinueTurns
        }
        Key::Char('n') | Key::Char('N') if matches!(overlay, Overlay::TurnLimit { .. }) => {
            OverlayAction::StopTurns
        }
        Key::Char(c) => {
            if let Overlay::Slash { filter, .. } = overlay {
                let mut f = filter.clone();
                f.push(c);
                overlay.filter_slash(&f);
            }
            OverlayAction::None
        }
        Key::Backspace => {
            if let Overlay::Slash { filter, .. } = overlay {
                let mut f = filter.clone();
                f.pop();
                overlay.filter_slash(&f);
            }
            OverlayAction::None
        }
        _ => OverlayAction::None,
    }
}

fn hitl_args(args: &serde_json::Value) -> String {
    let value = args
        .get("command")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| serde_json::to_string(args).unwrap_or_else(|_| "{}".into()));
    value.chars().take(240).collect()
}

fn parent_dir(path: &str) -> Option<String> {
    Path::new(path)
        .parent()
        .map(|path| path.display().to_string())
}

/// Minimal key enum for testable overlay handling (mapped from crossterm in app).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Esc,
    Enter,
    Up,
    Down,
    Left,
    Right,
    Backspace,
    Char(char),
    /// Bracketed-paste payload (full string at once).
    Paste(String),
    Other,
}

pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup[1])[1]
}

pub struct OverlayWidget<'a> {
    pub overlay: &'a Overlay,
}

impl Widget for OverlayWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if !matches!(self.overlay, Overlay::Hitl { .. }) {
            Clear.render(area, buf);
        }
        match self.overlay {
            Overlay::Welcome => {
                let r = centered_rect(64, 58, area);
                Paragraph::new(
                    "Your terminal-native coding agent. Let's get you ready in two quick steps.\n\n1  Connect a model provider\n   Sign in or add an API key using secure credential storage.\n\n2  Choose your model\n   Pick a default from the provider's available models.\n\nForge works in your current directory and asks before sensitive actions.\n\nPress Enter to get started  ·  Esc to explore without connecting",
                )
                .wrap(ratatui::widgets::Wrap { trim: true })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::brand())
                        .style(theme::panel())
                        .title(Span::styled(" Welcome to Forge ", theme::brand())),
                )
                .render(r, buf);
            }
            Overlay::TurnLimit { turns } => {
                let r = centered_rect(52, 24, area);
                let body = format!(
                    "The agent used {turns} model steps and still has work to do.\n\nContinue for another {turns} steps?\n\n[y/Enter] Continue    [n/Esc] Stop"
                );
                Paragraph::new(body)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::warn())
                            .title(Span::styled(
                                " Turn limit reached ",
                                theme::warn().add_modifier(Modifier::BOLD),
                            )),
                    )
                    .render(r, buf);
            }
            Overlay::Hitl { payload } => {
                let r = centered_rect(56, 38, area);
                let args = hitl_args(&payload.args_redacted);
                let body = format!(
                    "Human approval required\n\nTool:  {}\nArgs:  {args}\nWhy:   {}\n\n\
Secrets are not shown. The process may exit; approve later with the same session id — the journal resumes without redoing completed steps.\n\n\
[a] Approve once    [s] Allow for session\n[d] Deny            [Esc] Dismiss · remains pending",
                    payload.tool, payload.reason
                );
                Paragraph::new(body)
                    .wrap(ratatui::widgets::Wrap { trim: true })
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::warn())
                            .style(theme::panel())
                            .title(Span::styled(
                                format!(" HITL · high-risk · {} ", payload.tool),
                                theme::warn().add_modifier(Modifier::BOLD),
                            )),
                    )
                    .render(r, buf);
            }
            Overlay::Slash {
                filter,
                selected,
                items,
            } => {
                let r = centered_rect(50, 50, area);
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::border())
                    .style(theme::panel())
                    .title(Span::styled(format!(" / {filter} "), theme::brand()));
                let inner = block.inner(r);
                block.render(r, buf);
                let list_items: Vec<ListItem> = items
                    .iter()
                    .enumerate()
                    .map(|(i, it)| {
                        let marker = if i == *selected { "▶ " } else { "  " };
                        let style = if i == *selected {
                            theme::selected_row()
                        } else {
                            theme::text()
                        };
                        // Pad for full-width selection background
                        let mut row = format!("{marker}{:<12} {}", it.cmd, it.desc);
                        while row.chars().count() < 40 {
                            row.push(' ');
                        }
                        ListItem::new(Span::styled(row, style))
                    })
                    .collect();
                List::new(list_items).render(inner, buf);
            }
            Overlay::Model {
                provider_selected,
                model_selected,
                model_input,
                providers,
                items,
            } => {
                let r = centered_rect(70, 55, area);
                let pid = providers
                    .get(*provider_selected)
                    .map(|s| s.as_str())
                    .unwrap_or("all");
                let filtered: Vec<&ModelItem> = items
                    .iter()
                    .filter(|m| {
                        model_matches_provider(pid, m) && model_matches_input(model_input, m)
                    })
                    .collect();
                let total = filtered.len();
                let visible = r.height.saturating_sub(4).max(1) as usize;
                let start = if *model_selected < visible {
                    0
                } else if *model_selected + 1 > visible {
                    (*model_selected).saturating_add(1).saturating_sub(visible)
                } else {
                    0
                };
                let end = (start + visible).min(filtered.len());
                let window = &filtered[start..end];
                let list_items: Vec<ListItem> = if window.is_empty() {
                    vec![ListItem::new(Span::styled(
                        if items.is_empty() {
                            "No connected provider models. Use /connect first."
                        } else {
                            "No models match this provider or filter."
                        },
                        theme::muted(),
                    ))]
                } else {
                    window
                        .iter()
                        .enumerate()
                        .map(|(i, m)| {
                            let idx = start + i;
                            let marker = if idx == *model_selected { "▶ " } else { "  " };
                            let style = if idx == *model_selected {
                                theme::selected_row()
                            } else {
                                theme::text()
                            };
                            let tag = m
                                .profile_id
                                .as_deref()
                                .map(|p| format!("  [{p}]"))
                                .unwrap_or_default();
                            let mut row = format!("{marker}{}{tag}", m.model);
                            while row.chars().count() < 40 {
                                row.push(' ');
                            }
                            ListItem::new(Span::styled(row, style))
                        })
                        .collect()
                };
                let prov_hint = if providers.len() > 1 {
                    " · ←/→ provider"
                } else {
                    ""
                };
                let page = if total == 0 {
                    "0/0".into()
                } else {
                    format!("{}/{}", (*model_selected + 1).min(total), total)
                };
                let title = format!(" Choose a model · {pid} · {page}{prov_hint} · Enter use ");
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::border())
                    .style(theme::panel())
                    .title(Span::styled(title, theme::brand()));
                let inner = block.inner(r);
                block.render(r, buf);
                let regions = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(2), Constraint::Min(1)])
                    .split(inner);
                Paragraph::new(format!("Model: {model_input}█"))
                    .style(theme::text())
                    .render(regions[0], buf);
                List::new(list_items).render(regions[1], buf);
            }
            Overlay::ConnectApiKey {
                title,
                auth_url,
                key_input,
                env_hint,
                error,
                ..
            } => {
                let r = centered_rect(66, 42, area);
                let masked: String = "•".repeat(key_input.chars().count());
                let url = auth_url.as_deref().unwrap_or("(see docs)");
                let n = key_input.chars().count();
                let env_line = env_hint
                    .as_ref()
                    .map(|h| format!("\n[e] Use {h}"))
                    .unwrap_or_default();
                let error_line = error
                    .as_ref()
                    .map(|e| format!("\n\nCould not connect: {e}"))
                    .unwrap_or_default();
                let body = format!(
                    "{title}\n\nCreate or manage keys:\n{url}\n\nPaste API key:\n{masked}█  ({n} chars){error_line}\n\nEnter connect · Esc back{env_line}"
                );
                Paragraph::new(body)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::border())
                            .style(theme::panel())
                            .title(Span::styled(
                                " Connect with API key ",
                                theme::brand().add_modifier(Modifier::BOLD),
                            )),
                    )
                    .render(r, buf);
            }
            Overlay::ConnectOauth {
                title,
                instructions,
                ..
            } => {
                let r = centered_rect(70, 50, area);
                let body = format!(
                    "{title}\n\n{instructions}\n\nWaiting for sign-in…\nEnter check now · Esc back"
                );
                Paragraph::new(body)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::border())
                            .style(theme::panel())
                            .title(Span::styled(" Sign in ", theme::brand())),
                    )
                    .render(r, buf);
            }
            Overlay::ResumePicker { selected, items } => {
                let r = centered_rect(70, 48, area);
                let visible = r.height.saturating_sub(2).max(1) as usize;
                let start = selected
                    .saturating_add(1)
                    .saturating_sub(visible)
                    .min(items.len().saturating_sub(visible));
                let list_items: Vec<ListItem> = items
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(visible)
                    .map(|(index, item)| {
                        let marker = if index == *selected { "▶ " } else { "  " };
                        let style = if index == *selected {
                            theme::selected_row()
                        } else {
                            theme::text()
                        };
                        let row = format!("{marker}{}  ·  {}", item.id, item.modified);
                        ListItem::new(Span::styled(row, style))
                    })
                    .collect();
                List::new(list_items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::border())
                            .style(theme::panel())
                            .title(Span::styled(
                                " Resume a session · ↑↓ Enter · Esc cancel ",
                                theme::brand(),
                            )),
                    )
                    .render(r, buf);
            }
            Overlay::FileExplorer {
                cwd,
                selected,
                items,
                error,
            } => {
                let r = centered_rect(76, 64, area);
                let visible = r.height.saturating_sub(4).max(1) as usize;
                let start = selected
                    .saturating_add(1)
                    .saturating_sub(visible)
                    .min(items.len().saturating_sub(visible));
                let list_items: Vec<ListItem> = items
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(visible)
                    .map(|(index, item)| {
                        let marker = if index == *selected { "▶ " } else { "  " };
                        let style = if index == *selected {
                            theme::selected_row()
                        } else {
                            theme::text()
                        };
                        let kind = if item.is_dir { "📁" } else { "  " };
                        ListItem::new(Span::styled(format!("{marker}{kind} {}", item.name), style))
                    })
                    .collect();
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::border())
                    .style(theme::panel())
                    .title(Span::styled(
                        " File explorer · readonly · ↑↓ Enter · ←/Backspace up · Esc close ",
                        theme::brand(),
                    ));
                let inner = block.inner(r);
                block.render(r, buf);
                let regions = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(1),
                        Constraint::Min(1),
                        Constraint::Length(1),
                    ])
                    .split(inner);
                Paragraph::new(cwd.as_str())
                    .style(theme::muted())
                    .render(regions[0], buf);
                List::new(list_items).render(regions[1], buf);
                let status = error
                    .as_deref()
                    .unwrap_or("Enter opens directories/files · ←/Backspace moves up");
                Paragraph::new(status)
                    .style(theme::muted())
                    .render(regions[2], buf);
            }
            Overlay::FileViewer {
                path,
                lines,
                scroll,
            } => {
                let r = centered_rect(86, 78, area);
                let visible = r.height.saturating_sub(2).max(1) as usize;
                let width = r.width.saturating_sub(2) as usize;
                let body = lines
                    .iter()
                    .enumerate()
                    .skip(*scroll)
                    .take(visible)
                    .map(|(index, line)| {
                        let mut row = format!("{:>4} │ {}", index + 1, line);
                        row.truncate(width);
                        row
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let title = format!(
                    " {} · readonly · {}/{} · ↑↓ scroll · ←/Backspace back · Esc close ",
                    path,
                    (*scroll + 1).min(lines.len().max(1)),
                    lines.len().max(1)
                );
                Paragraph::new(body)
                    .style(theme::text())
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::border())
                            .style(theme::panel())
                            .title(Span::styled(title, theme::brand())),
                    )
                    .render(r, buf);
            }
            Overlay::ConnectPicker { selected, items } => {
                let r = centered_rect(58, 42, area);
                let list_items: Vec<ListItem> = items
                    .iter()
                    .enumerate()
                    .map(|(i, it)| {
                        let marker = if i == *selected { "▶ " } else { "  " };
                        let style = if i == *selected {
                            theme::selected_row()
                        } else {
                            theme::text()
                        };
                        let mode = match it.auth_mode.as_str() {
                            "oauth" => "Sign in",
                            "api_key" if it.id == "ollama" => "Local",
                            "api_key" => "API key",
                            other => other,
                        };
                        let state = if it.connected { "✓ connected" } else { mode };
                        let mut row = format!("{marker}{:<30} {state}", it.title);
                        while row.chars().count() < 48 {
                            row.push(' ');
                        }
                        ListItem::new(Span::styled(row, style))
                    })
                    .collect();
                List::new(list_items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::border())
                            .style(theme::panel())
                            .title(Span::styled(
                                " Choose a provider · ↑↓ Enter ",
                                theme::brand(),
                            )),
                    )
                    .render(r, buf);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{parse_slash, SlashCommand};
    use forge_types::HitlPayload;
    use serde_json::json;

    #[test]
    fn welcome_starts_onboarding() {
        let mut overlay = Overlay::welcome();
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::BeginOnboarding
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Esc),
            OverlayAction::Close
        );
    }

    #[test]
    fn filter_palette_narrows() {
        let items = filter_palette("app");
        assert!(items.is_empty());
        assert!(!items.iter().any(|i| i.cmd == "/quit"));
    }

    #[test]
    fn default_palette_covers_parseable_commands() {
        let items = default_palette_items();
        // Every bare cmd (no required args) should parse; arg-required ones still listed.
        for it in &items {
            let res = parse_slash(&it.cmd).expect("is slash");
            match it.cmd.as_str() {
                "/resume" => assert_eq!(res.unwrap(), SlashCommand::ResumeList),
                other => assert!(res.is_ok(), "palette cmd {other} should parse: {res:?}"),
            }
        }
        assert!(
            items.len() >= 14,
            "expected full command list, got {}",
            items.len()
        );
    }

    #[test]
    fn hitl_keys() {
        let mut o = Overlay::hitl(HitlPayload {
            call_id: "1".into(),
            tool: "bash".into(),
            args_redacted: json!({"command": "git push"}),
            reason: "policy".into(),
        });
        assert_eq!(
            handle_overlay_key(&mut o, Key::Char('a')),
            OverlayAction::HitlApprove
        );
        assert_eq!(
            handle_overlay_key(&mut o, Key::Char('d')),
            OverlayAction::HitlDeny
        );
        assert_eq!(handle_overlay_key(&mut o, Key::Esc), OverlayAction::Close);
    }

    #[test]
    fn hitl_args_prefers_redacted_command() {
        assert_eq!(
            hitl_args(&json!({"command": "git push -u origin feature"})),
            "git push -u origin feature"
        );
        assert_eq!(
            hitl_args(&json!({"path": "src/lib.rs"})),
            r#"{"path":"src/lib.rs"}"#
        );
    }

    #[test]
    fn turn_limit_keys_continue_or_stop() {
        let mut overlay = Overlay::turn_limit(128);
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::ContinueTurns
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Char('y')),
            OverlayAction::ContinueTurns
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Char('n')),
            OverlayAction::StopTurns
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Esc),
            OverlayAction::StopTurns
        );
    }

    #[test]
    fn slash_enter_runs_status() {
        let mut o = Overlay::slash_open("");
        // find /status index
        if let Overlay::Slash {
            selected, items, ..
        } = &mut o
        {
            *selected = items.iter().position(|i| i.cmd == "/status").unwrap();
        }
        let a = handle_overlay_key(&mut o, Key::Enter);
        assert_eq!(a, OverlayAction::RunCommand("/status".into()));
    }

    #[test]
    fn model_select() {
        let mut o = Overlay::model_open();
        let a = handle_overlay_key(&mut o, Key::Enter);
        match a {
            OverlayAction::SelectModel { provider, model } => {
                assert_eq!(provider, "native");
                assert!(model.contains('/') || !model.is_empty());
            }
            _ => panic!("expected model select"),
        }
    }

    #[test]
    fn model_accepts_typed_custom_model() {
        let mut overlay = Overlay::model_open();
        for c in "openai/custom-model".chars() {
            assert_eq!(
                handle_overlay_key(&mut overlay, Key::Char(c)),
                OverlayAction::None
            );
        }
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::RunCommand("/model openai/custom-model".into())
        );
    }

    #[test]
    fn model_input_supports_paste_and_backspace() {
        let mut overlay = Overlay::model_open();
        handle_overlay_key(&mut overlay, Key::Paste("anthropic/custom-modelx\n".into()));
        handle_overlay_key(&mut overlay, Key::Backspace);
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::RunCommand("/model anthropic/custom-model".into())
        );
    }

    #[test]
    fn palette_moves() {
        let mut o = Overlay::slash_open("");
        handle_overlay_key(&mut o, Key::Down);
        if let Overlay::Slash { selected, .. } = o {
            assert_eq!(selected, 1);
        } else {
            panic!();
        }
    }

    #[test]
    fn resume_picker_moves_and_runs_selected_session() {
        let mut overlay = Overlay::resume_picker(vec![
            ResumeSessionItem {
                id: "first".into(),
                modified: "newest".into(),
            },
            ResumeSessionItem {
                id: "second".into(),
                modified: "older".into(),
            },
        ]);
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Down),
            OverlayAction::None
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::RunCommand("/resume second".into())
        );
    }

    #[test]
    fn connect_api_key_overlay_requires_key_and_masks() {
        let mut o = Overlay::connect_api_key(
            "opencode_go",
            "OpenCode Go",
            Some("https://opencode.ai/auth".into()),
            None,
        );
        // Enter with empty key does nothing
        assert_eq!(handle_overlay_key(&mut o, Key::Enter), OverlayAction::None);
        handle_overlay_key(&mut o, Key::Char('s'));
        handle_overlay_key(&mut o, Key::Char('e'));
        handle_overlay_key(&mut o, Key::Char('c'));
        let a = handle_overlay_key(&mut o, Key::Enter);
        match a {
            OverlayAction::ConnectSubmitKey {
                profile_id,
                api_key,
            } => {
                assert_eq!(profile_id, "opencode_go");
                assert_eq!(api_key, "sec");
            }
            other => panic!("expected submit key, got {other:?}"),
        }
        if let Overlay::ConnectApiKey { key_input, .. } = &o {
            assert_eq!(key_input, "sec");
        }
    }

    #[test]
    fn connect_api_key_e_types_into_field_even_when_env_hint() {
        // Regression: plain 'e' used to fire ConnectUseEnv and abort paste mid-key.
        let mut o = Overlay::connect_api_key(
            "opencode_go",
            "OpenCode Go",
            Some("https://opencode.ai/auth".into()),
            Some("OPENCODE_API_KEY".into()),
        );
        for c in "sk-test-key-with-e-chars".chars() {
            let a = handle_overlay_key(&mut o, Key::Char(c));
            assert_eq!(a, OverlayAction::None, "char {c:?} must not steal env");
        }
        if let Overlay::ConnectApiKey { key_input, .. } = &o {
            assert_eq!(key_input, "sk-test-key-with-e-chars");
        } else {
            panic!("expected ConnectApiKey");
        }
        let a = handle_overlay_key(&mut o, Key::Enter);
        match a {
            OverlayAction::ConnectSubmitKey { api_key, .. } => {
                assert_eq!(api_key, "sk-test-key-with-e-chars");
            }
            other => panic!("expected submit, got {other:?}"),
        }
    }

    #[test]
    fn connect_api_key_e_uses_env_only_when_field_empty() {
        let mut o = Overlay::connect_api_key(
            "opencode_go",
            "OpenCode Go",
            None,
            Some("OPENCODE_API_KEY".into()),
        );
        let a = handle_overlay_key(&mut o, Key::Char('e'));
        assert_eq!(
            a,
            OverlayAction::ConnectUseEnv {
                profile_id: "opencode_go".into()
            }
        );
    }

    #[test]
    fn connect_api_key_paste_inserts_full_string() {
        let mut o = Overlay::connect_api_key("opencode_go", "OpenCode Go", None, None);
        let pasted = "abc123XYZ-long-api-key-value\n";
        handle_overlay_key(&mut o, Key::Paste(pasted.into()));
        if let Overlay::ConnectApiKey { key_input, .. } = &o {
            assert_eq!(key_input, "abc123XYZ-long-api-key-value");
            assert_eq!(key_input.chars().count(), 28);
        } else {
            panic!("expected ConnectApiKey");
        }
    }

    #[test]
    fn connect_oauth_overlay_enter_completes() {
        let mut o = Overlay::connect_oauth("xai", "xAI Grok", "Visit accounts.x.ai");
        let a = handle_overlay_key(&mut o, Key::Enter);
        assert_eq!(
            a,
            OverlayAction::ConnectCompleteOauth {
                profile_id: "xai".into()
            }
        );
    }

    #[test]
    fn file_explorer_left_moves_to_parent() {
        let mut o = Overlay::file_explorer("/workspace/src", vec![], None);
        let a = handle_overlay_key(&mut o, Key::Left);
        assert_eq!(
            a,
            OverlayAction::FilePick {
                path: "/workspace".into(),
                is_dir: true,
            }
        );
    }

    #[test]
    fn file_viewer_backspace_returns_to_containing_dir() {
        let mut o = Overlay::file_viewer("/workspace/src/lib.rs", "contents");
        let a = handle_overlay_key(&mut o, Key::Backspace);
        assert_eq!(
            a,
            OverlayAction::FilePick {
                path: "/workspace/src".into(),
                is_dir: true,
            }
        );
    }

    #[test]
    fn connect_picker_sorts_providers_alphabetically() {
        let overlay = Overlay::connect_picker(vec![
            ConnectProfileItem {
                id: "xai".into(),
                title: "xAI Grok".into(),
                auth_mode: "oauth".into(),
                auth_url: None,
                connected: false,
            },
            ConnectProfileItem {
                id: "anthropic".into(),
                title: "Anthropic".into(),
                auth_mode: "api_key".into(),
                auth_url: None,
                connected: false,
            },
            ConnectProfileItem {
                id: "openai".into(),
                title: "OpenAI".into(),
                auth_mode: "api_key".into(),
                auth_url: None,
                connected: false,
            },
        ]);

        let Overlay::ConnectPicker { items, .. } = overlay else {
            panic!("expected connect picker");
        };
        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["anthropic", "openai", "xai"]
        );
    }
}
