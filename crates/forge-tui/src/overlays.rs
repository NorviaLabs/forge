//! Overlays: HITL, slash palette, model picker (TUI-04).

use crate::theme;
use forge_types::HitlPayload;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
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
    vec![
        PaletteItem {
            cmd: "/help".into(),
            desc: "List commands".into(),
        },
        PaletteItem {
            cmd: "/status".into(),
            desc: "Session status".into(),
        },
        PaletteItem {
            cmd: "/resume".into(),
            desc: "Resume session by id".into(),
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
            cmd: "/reset".into(),
            desc: "Force context handoff".into(),
        },
        PaletteItem {
            cmd: "/approve".into(),
            desc: "Approve HITL".into(),
        },
        PaletteItem {
            cmd: "/deny".into(),
            desc: "Deny HITL".into(),
        },
        PaletteItem {
            cmd: "/worktree".into(),
            desc: "status|merge|discard".into(),
        },
        PaletteItem {
            cmd: "/model".into(),
            desc: "Switch provider/model".into(),
        },
        PaletteItem {
            cmd: "/connect".into(),
            desc: "Connect xAI Grok or OpenCode Go".into(),
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
    HitlDeny,
    /// Execute slash command string e.g. "/status"
    RunCommand(String),
    /// Insert into input
    InsertInput(String),
    /// Model selection
    SelectModel { provider: String, model: String },
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
                        "/help" | "/status" | "/tools" | "/cost" | "/quit" | "/approve" | "/deny" | "/reset" | "/compact"
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
            Overlay::Hitl { .. } => OverlayAction::None,
        },
        Key::Char('a') | Key::Char('A') if matches!(overlay, Overlay::Hitl { .. }) => {
            OverlayAction::HitlApprove
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Esc,
    Enter,
    Up,
    Down,
    Backspace,
    Char(char),
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
                    "Tool:  {}\nCall:  {}\nWhy:   {}\n\nArgs (redacted):\n{args}\n\n[a] Approve    [d] Deny    [Esc] dismiss",
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
                        let style = if i == *selected {
                            theme::brand().add_modifier(Modifier::BOLD)
                        } else {
                            theme::text()
                        };
                        ListItem::new(Line::from(vec![
                            Span::styled(format!("{:<12}", it.cmd), style),
                            Span::styled(it.desc.clone(), theme::muted()),
                        ]))
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
                        let style = if i == *selected {
                            theme::brand().add_modifier(Modifier::BOLD)
                        } else {
                            theme::text()
                        };
                        ListItem::new(Span::styled(
                            format!("{} / {}", m.provider, m.model),
                            style,
                        ))
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
}
