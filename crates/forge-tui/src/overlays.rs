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
    Hitl { payload: HitlPayload },
    Slash {
        filter: String,
        selected: usize,
        items: Vec<PaletteItem>,
    },
    Model {
        selected: usize,
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
}

#[derive(Debug, Clone)]
pub struct ConnectProfileItem {
    pub id: String,
    pub title: String,
    pub auth_mode: String,
    pub auth_url: Option<String>,
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
}

pub fn default_palette_items() -> Vec<PaletteItem> {
    // Keep in sync with `commands::parse_slash` / `help_text`.
    vec![
        PaletteItem {
            cmd: "/help".into(),
            desc: "List all commands".into(),
        },
        PaletteItem {
            cmd: "/status".into(),
            desc: "Session status".into(),
        },
        PaletteItem {
            cmd: "/connect".into(),
            desc: "Connect xAI Grok or OpenCode Go".into(),
        },
        PaletteItem {
            cmd: "/model".into(),
            desc: "Switch provider/model".into(),
        },
        PaletteItem {
            cmd: "/tools".into(),
            desc: "List tools".into(),
        },
        PaletteItem {
            cmd: "/cost".into(),
            desc: "Context usage".into(),
        },
        PaletteItem {
            cmd: "/journal".into(),
            desc: "Tail journal events".into(),
        },
        PaletteItem {
            cmd: "/worktree".into(),
            desc: "status | merge | discard".into(),
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
            cmd: "/reset".into(),
            desc: "Force context handoff".into(),
        },
        PaletteItem {
            cmd: "/compact".into(),
            desc: "Alias for /reset".into(),
        },
        PaletteItem {
            cmd: "/resume".into(),
            desc: "Resume session by uuid".into(),
        },
        PaletteItem {
            cmd: "/cancel".into(),
            desc: "Soft-cancel current turn".into(),
        },
        PaletteItem {
            cmd: "/diff".into(),
            desc: "Tools & file changes".into(),
        },
        PaletteItem {
            cmd: "/copy".into(),
            desc: "Copy last answer".into(),
        },
        PaletteItem {
            cmd: "/clear".into(),
            desc: "Clear banners / notices".into(),
        },
        PaletteItem {
            cmd: "/density".into(),
            desc: "Toggle compact layout".into(),
        },
        PaletteItem {
            cmd: "/quit".into(),
            desc: "Exit TUI".into(),
        },
    ]
}

pub fn default_models() -> Vec<ModelItem> {
    // Phase 5/6: LiteLLM model strings (provider field = litellm routing label)
    let mut items = vec![
        ModelItem {
            provider: "litellm".into(),
            model: "openai/gpt-4.1-mini".into(),
        },
        ModelItem {
            provider: "litellm".into(),
            model: "anthropic/claude-sonnet".into(),
        },
    ];
    for p in forge_connect::builtin_registry().profiles() {
        for m in &p.default_models {
            items.push(ModelItem {
                provider: "litellm".into(),
                model: m.clone(),
            });
        }
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
        Self::Model {
            selected: 0,
            items: default_models(),
        }
    }

    pub fn hitl(payload: HitlPayload) -> Self {
        Self::Hitl { payload }
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
        Self::ConnectPicker { selected: 0, items }
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
            Self::Slash { selected, items, .. } => {
                if items.is_empty() {
                    return;
                }
                let n = items.len() as i32;
                *selected = ((*selected as i32 + delta).rem_euclid(n)) as usize;
            }
            Self::Model { selected, items, .. } => {
                if items.is_empty() {
                    return;
                }
                let n = items.len() as i32;
                *selected = ((*selected as i32 + delta).rem_euclid(n)) as usize;
            }
            Self::ConnectPicker { selected, items, .. } => {
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
    /// Execute slash command string e.g. "/status"
    RunCommand(String),
    /// Insert into input
    InsertInput(String),
    /// Model selection
    SelectModel { provider: String, model: String },
    /// Submit API key from ConnectApiKey overlay
    ConnectSubmitKey { profile_id: String, api_key: String },
    /// Poll / continue OAuth from ConnectOauth overlay (Enter)
    ConnectCompleteOauth { profile_id: String },
    /// Use env key without typing (secondary action on API key modal)
    ConnectUseEnv { profile_id: String },
    /// User picked a connect profile from the picker
    ConnectPickProfile { profile_id: String },
}

pub fn handle_overlay_key(overlay: &mut Overlay, key: Key) -> OverlayAction {
    match key {
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
        Key::Enter => match overlay {
            Overlay::Slash {
                selected, items, ..
            } => {
                if let Some(item) = items.get(*selected) {
                    let cmd = item.cmd.clone();
                    // no-arg commands execute; others insert
                    if matches!(
                        cmd.as_str(),
                        "/help"
                            | "/status"
                            | "/tools"
                            | "/cost"
                            | "/quit"
                            | "/approve"
                            | "/deny"
                            | "/reset"
                            | "/compact"
                            | "/diff"
                            | "/copy"
                            | "/clear"
                            | "/density"
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
                selected, items, ..
            } => {
                if let Some(m) = items.get(*selected) {
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
                    .title(Span::styled(
                        format!(" / {filter} "),
                        theme::brand(),
                    ));
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
            Overlay::Model { selected, items } => {
                let r = centered_rect(50, 40, area);
                let list_items: Vec<ListItem> = items
                    .iter()
                    .enumerate()
                    .map(|(i, m)| {
                        let marker = if i == *selected { "▶ " } else { "  " };
                        let style = if i == *selected {
                            theme::selected_row()
                        } else {
                            theme::text()
                        };
                        let mut row = format!("{marker}{} / {}", m.provider, m.model);
                        while row.chars().count() < 36 {
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
                            .title(Span::styled(" model picker ", theme::muted())),
                    )
                    .render(r, buf);
            }
            Overlay::ConnectApiKey {
                title,
                auth_url,
                key_input,
                env_hint,
                ..
            } => {
                let r = centered_rect(70, 45, area);
                let masked: String = "*".repeat(key_input.chars().count());
                let url = auth_url.as_deref().unwrap_or("(see docs)");
                let n = key_input.chars().count();
                let env_line = env_hint
                    .as_ref()
                    .map(|h| {
                        format!(
                            "\n[e] Use existing env ({h}) — only while field is empty"
                        )
                    })
                    .unwrap_or_default();
                let body = format!(
                    "Connect: {title}\n\n1. Sign in and copy your API key:\n   {url}\n\n2. Paste API key below (masked):\n   [{masked}]\n   ({n} chars)\n\n[Enter] Connect    [Esc] Cancel{env_line}"
                );
                Paragraph::new(body)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::brand())
                            .title(Span::styled(
                                " API key required ",
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
                    "Connect: {title} (OAuth)\n\nAPI keys are not used for this profile.\n\n{instructions}\n\n[Enter] Complete (fixture/env)    [Esc] Cancel"
                );
                Paragraph::new(body)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::info())
                            .title(Span::styled(" OAuth ", theme::info())),
                    )
                    .render(r, buf);
            }
            Overlay::ConnectPicker { selected, items } => {
                let r = centered_rect(60, 45, area);
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
                        let url = it.auth_url.as_deref().unwrap_or("");
                        let mut row =
                            format!("{marker}{} ({})  {url}", it.title, it.auth_mode);
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
                            .title(Span::styled(
                                " /connect — select profile ↑↓ Enter ",
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
                "/resume" => assert!(res.is_err(), "bare /resume needs uuid"),
                other => assert!(
                    res.is_ok(),
                    "palette cmd {other} should parse: {res:?}"
                ),
            }
        }
        assert!(items.len() >= 14, "expected full command list, got {}", items.len());
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
                assert_eq!(provider, "litellm");
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
}
