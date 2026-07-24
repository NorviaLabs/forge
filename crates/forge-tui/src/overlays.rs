//! Overlays: HITL, slash palette, model picker (TUI-04).

use crate::theme;
use forge_types::HitlPayload;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Widget};

#[derive(Debug, Clone)]
pub enum Overlay {
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
            desc: "Session status".into(),
        },
        PaletteItem {
            cmd: "/connect".into(),
            desc: "Connect provider (xAI, OpenCode Go/Zen, OpenAI, Anthropic, Ollama)".into(),
        },
        PaletteItem {
            cmd: "/model".into(),
            desc: "Switch model (catalog)".into(),
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
            cmd: "/approve".into(),
            desc: "Approve pending HITL".into(),
        },
        PaletteItem {
            cmd: "/deny".into(),
            desc: "Deny pending HITL".into(),
        },
        PaletteItem {
            cmd: "/compact".into(),
            desc: "Force context handoff".into(),
        },
        PaletteItem {
            cmd: "/resume".into(),
            desc: "List recent sessions to resume".into(),
        },
        PaletteItem {
            cmd: "/diff".into(),
            desc: "Tools & file changes".into(),
        },
        PaletteItem {
            cmd: "/sync".into(),
            desc: "Commit + push (message from changeset)".into(),
        },
        PaletteItem {
            cmd: "/stt".into(),
            desc: "STT status / speed (Ctrl+Space PTT)".into(),
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

/// Build picker rows from catalog entries (live/cached) + optional fallbacks.
pub fn models_from_catalog(entries: &[forge_connect::CatalogEntry]) -> Vec<ModelItem> {
    let mut items: Vec<ModelItem> = entries
        .iter()
        .map(|e| ModelItem {
            provider: "native".into(),
            model: e.id.clone(),
            profile_id: Some(e.profile_id.clone()),
        })
        .collect();
    if items.is_empty() {
        items = default_models();
    }
    items
}

impl Overlay {
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
                let n = filtered_models_len(pid, items).max(1) as i32;
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

fn filtered_models_len(provider_id: &str, items: &[ModelItem]) -> usize {
    items
        .iter()
        .filter(|m| model_matches_provider(provider_id, m))
        .count()
}

pub fn filter_palette(filter: &str) -> Vec<PaletteItem> {
    let f = filter.trim().trim_start_matches('/').to_ascii_lowercase();
    default_palette_items()
        .into_iter()
        .filter(|i| {
            if f.is_empty() {
                return true;
            }
            i.cmd.to_ascii_lowercase().contains(&f) || i.desc.to_ascii_lowercase().contains(&f)
        })
        .collect()
}

/// Result of handling a key inside an overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayAction {
    None,
    Close,
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
                            | "/approve"
                            | "/deny"
                            | "/compact"
                            | "/diff"
                            | "/sync"
                            | "/stt"
                            | "/copy"
                            | "/clear"
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
                providers,
                items,
            } => {
                let pid = providers
                    .get(*provider_selected)
                    .map(|s| s.as_str())
                    .unwrap_or("all");
                let chosen = items
                    .iter()
                    .filter(|m| model_matches_provider(pid, m))
                    .nth(*model_selected);
                if let Some(m) = chosen {
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
        Key::Backspace if matches!(overlay, Overlay::ConnectApiKey { .. }) => {
            if let Overlay::ConnectApiKey { key_input, .. } = overlay {
                key_input.pop();
            }
            OverlayAction::None
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
        // dim full area
        Clear.render(area, buf);
        match self.overlay {
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
                let r = centered_rect(60, 40, area);
                let args = serde_json::to_string_pretty(&payload.args_redacted)
                    .unwrap_or_else(|_| "{}".into());
                let args: String = args.chars().take(400).collect();
                let body = format!(
                    "Tool:  {}\nCall:  {}\nWhy:   {}\n\nArgs (redacted):\n{args}\n\n\
[a] Approve once    [s] Allow for session    [d] Deny    [Esc] dismiss",
                    payload.tool, payload.call_id, payload.reason
                );
                Paragraph::new(body)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::warn())
                            .title(Span::styled(
                                " Human approval required (HITL) ",
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
                    .filter(|m| model_matches_provider(pid, m))
                    .collect();
                let total = filtered.len();
                let visible = r.height.saturating_sub(2).max(1) as usize;
                let start = if *model_selected < visible {
                    0
                } else if *model_selected + 1 > visible {
                    (*model_selected).saturating_add(1).saturating_sub(visible)
                } else {
                    0
                };
                let end = (start + visible).min(filtered.len());
                let window = &filtered[start..end];
                let list_items: Vec<ListItem> = window
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
                    .collect();
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
                List::new(list_items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::border())
                            .style(theme::panel())
                            .title(Span::styled(title, theme::brand())),
                    )
                    .render(r, buf);
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
                            .border_style(theme::brand())
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
                            .border_style(theme::info())
                            .style(theme::panel())
                            .title(Span::styled(" Sign in ", theme::info())),
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
                            .border_style(theme::brand())
                            .style(theme::panel())
                            .title(Span::styled(
                                " Resume a session · ↑↓ Enter · Esc cancel ",
                                theme::brand(),
                            )),
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
                            .border_style(theme::brand())
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
    fn filter_palette_narrows() {
        let items = filter_palette("app");
        assert!(items.iter().any(|i| i.cmd.contains("approve")));
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
    fn parse_slash_still_works() {
        assert!(matches!(
            parse_slash("/approve").unwrap().unwrap(),
            SlashCommand::Approve
        ));
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
