//! Conversation view model (TUI-02 / tui-conversation.md).

use crate::theme;
use forge_core::{AgentSession, TurnEvent};
use forge_types::{Message, MessageRole, SessionStatus};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCardState {
    Running,
    Done,
    Blocked,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatItem {
    System { text: String },
    User { text: String },
    Assistant { text: String },
    ToolCard {
        name: String,
        summary: String,
        state: ToolCardState,
    },
    Banner { text: String, kind: BannerKind },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    Info,
    Warn,
    Error,
    Ok,
}

#[derive(Debug, Clone)]
pub struct ConversationModel {
    pub items: Vec<ChatItem>,
    pub scroll: u16,
    pub follow: bool,
    pub busy: bool,
}

impl ConversationModel {
    pub fn from_messages(
        messages: &[Message],
        events: &[TurnEvent],
        status: SessionStatus,
        busy: bool,
    ) -> Self {
        let mut items = Vec::new();
        for m in messages {
            match m.role {
                MessageRole::System => items.push(ChatItem::System {
                    text: m.content.clone(),
                }),
                MessageRole::User => {
                    if m.content.starts_with("[REPAIR TASK") {
                        items.push(ChatItem::Banner {
                            text: m.content.clone(),
                            kind: BannerKind::Warn,
                        });
                    } else {
                        items.push(ChatItem::User {
                            text: m.content.clone(),
                        });
                    }
                }
                MessageRole::Assistant => items.push(ChatItem::Assistant {
                    text: m.content.clone(),
                }),
                MessageRole::Tool => {
                    let name = m.name.clone().unwrap_or_else(|| "tool".into());
                    let (state, summary) = classify_tool_content(&m.content);
                    items.push(ChatItem::ToolCard {
                        name,
                        summary,
                        state,
                    });
                }
            }
        }
        for e in events {
            if e.kind == "context_reset" {
                items.push(ChatItem::Banner {
                    text: format!("context lifecycle: {}", e.detail),
                    kind: BannerKind::Warn,
                });
            } else if e.kind == "hitl_wait" {
                items.push(ChatItem::Banner {
                    text: format!("HITL required: {}", e.detail),
                    kind: BannerKind::Warn,
                });
            } else if e.kind == "validation" {
                items.push(ChatItem::Banner {
                    text: e.detail.clone(),
                    kind: BannerKind::Error,
                });
            }
        }
        if status == SessionStatus::AwaitingHitl {
            items.push(ChatItem::Banner {
                text: "Awaiting human approval — press a/d in modal or /approve /deny".into(),
                kind: BannerKind::Warn,
            });
        }
        if items.is_empty() {
            items.push(ChatItem::System {
                text: "Forge ready. Type a task, or / for commands.".into(),
            });
        }
        Self {
            items,
            scroll: 0,
            follow: true,
            busy,
        }
    }

    pub fn from_session(session: &AgentSession, busy: bool) -> Self {
        Self::from_messages(&session.messages, &session.events, session.status, busy)
    }

    /// Append an in-progress assistant bubble (token stream preview).
    pub fn with_streaming_assistant(mut self, text: impl Into<String>) -> Self {
        let text = text.into();
        if !text.is_empty() || self.busy {
            self.items.push(ChatItem::Assistant { text });
        }
        self
    }

    /// Phase 10: append UI-only banners (errors, info) after session-derived items.
    pub fn with_extra_banners(mut self, banners: impl IntoIterator<Item = ChatItem>) -> Self {
        for b in banners {
            self.items.push(b);
        }
        self
    }

    pub fn scroll_up(&mut self, n: u16) {
        self.follow = false;
        self.scroll = self.scroll.saturating_add(n);
    }

    pub fn scroll_down(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_sub(n);
        if self.scroll == 0 {
            self.follow = true;
        }
    }

    pub fn lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for item in &self.items {
            match item {
                ChatItem::System { text } => {
                    lines.push(Line::from(Span::styled("SYSTEM", theme::warn())));
                    for l in wrap(text, 100) {
                        lines.push(Line::from(Span::styled(l, theme::muted())));
                    }
                    lines.push(Line::from(""));
                }
                ChatItem::User { text } => {
                    lines.push(Line::from(Span::styled("YOU", theme::info())));
                    for l in wrap(text, 100) {
                        lines.push(Line::from(Span::styled(l, theme::text())));
                    }
                    lines.push(Line::from(""));
                }
                ChatItem::Assistant { text } => {
                    let label = if self.busy {
                        "ASSISTANT · working"
                    } else {
                        "ASSISTANT"
                    };
                    lines.push(Line::from(Span::styled(label, theme::brand())));
                    for l in wrap(text, 100) {
                        lines.push(Line::from(Span::styled(l, theme::text())));
                    }
                    lines.push(Line::from(""));
                }
                ChatItem::ToolCard {
                    name,
                    summary,
                    state,
                } => {
                    let (tag, st) = match state {
                        ToolCardState::Running => ("● running", theme::info()),
                        ToolCardState::Done => ("✓ done", theme::ok()),
                        ToolCardState::Blocked => ("⏸ blocked", theme::warn()),
                        ToolCardState::Error => ("✗ error", theme::danger()),
                    };
                    lines.push(Line::from(vec![
                        Span::styled("TOOL ", theme::tool()),
                        Span::styled(name.clone(), theme::tool().add_modifier(Modifier::BOLD)),
                        Span::raw("  "),
                        Span::styled(tag, st),
                    ]));
                    for l in wrap(summary, 96) {
                        lines.push(Line::from(Span::styled(format!("  {l}"), theme::muted())));
                    }
                    lines.push(Line::from(""));
                }
                ChatItem::Banner { text, kind } => {
                    let st = match kind {
                        BannerKind::Info => theme::info(),
                        BannerKind::Warn => theme::warn(),
                        BannerKind::Error => theme::danger(),
                        BannerKind::Ok => theme::ok(),
                    };
                    for l in wrap(text, 100) {
                        lines.push(Line::from(Span::styled(format!("▸ {l}"), st)));
                    }
                    lines.push(Line::from(""));
                }
            }
        }
        if self.busy {
            lines.push(Line::from(Span::styled(
                "… working …",
                theme::info().add_modifier(Modifier::ITALIC),
            )));
        }
        lines
    }
}

fn classify_tool_content(content: &str) -> (ToolCardState, String) {
    let lower = content.to_ascii_lowercase();
    let state = if lower.contains("validation") || lower.contains("denied by acl") {
        ToolCardState::Error
    } else if lower.contains("hitl") || lower.contains("awaiting") {
        ToolCardState::Blocked
    } else if content.contains("offloaded tool output") {
        ToolCardState::Done
    } else {
        ToolCardState::Done
    };
    let summary: String = content.chars().take(240).collect();
    (state, summary)
}

fn wrap(s: &str, width: usize) -> Vec<String> {
    if s.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    for para in s.lines() {
        if para.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        for word in para.split_whitespace() {
            if cur.is_empty() {
                cur = word.to_string();
            } else if cur.len() + 1 + word.len() <= width {
                cur.push(' ');
                cur.push_str(word);
            } else {
                out.push(cur);
                cur = word.to_string();
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

pub struct ConversationWidget<'a> {
    pub model: &'a ConversationModel,
}

impl Widget for ConversationWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let lines = self.model.lines();
        let total = lines.len() as u16;
        let height = area.height.saturating_sub(2);
        let max_scroll = total.saturating_sub(height);
        let scroll = if self.model.follow {
            max_scroll
        } else {
            self.model.scroll.min(max_scroll)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border())
            .title(Span::styled(" conversation ", theme::muted()));
        Paragraph::new(lines)
            .block(block)
            .scroll((scroll, 0))
            .render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::Message;

    #[test]
    fn roles_map_to_items() {
        let msgs = vec![
            Message {
                role: MessageRole::System,
                content: "sys".into(),
                tool_call_id: None,
                name: None,
            },
            Message {
                role: MessageRole::User,
                content: "hi".into(),
                tool_call_id: None,
                name: None,
            },
            Message {
                role: MessageRole::Assistant,
                content: "yo".into(),
                tool_call_id: None,
                name: None,
            },
            Message {
                role: MessageRole::Tool,
                content: "ok body".into(),
                tool_call_id: Some("1".into()),
                name: Some("read_file".into()),
            },
        ];
        let m = ConversationModel::from_messages(&msgs, &[], SessionStatus::Running, false);
        assert!(matches!(m.items[0], ChatItem::System { .. }));
        assert!(matches!(m.items[1], ChatItem::User { .. }));
        assert!(matches!(m.items[2], ChatItem::Assistant { .. }));
        assert!(matches!(
            m.items[3],
            ChatItem::ToolCard {
                state: ToolCardState::Done,
                ..
            }
        ));
    }

    #[test]
    fn validation_is_error_card() {
        let msgs = vec![Message {
            role: MessageRole::Tool,
            content: "Tool validation error: bad".into(),
            tool_call_id: Some("1".into()),
            name: Some("read_file".into()),
        }];
        let m = ConversationModel::from_messages(&msgs, &[], SessionStatus::Running, false);
        match &m.items[0] {
            ChatItem::ToolCard { state, .. } => assert_eq!(*state, ToolCardState::Error),
            _ => panic!("expected tool card"),
        }
    }

    #[test]
    fn empty_shows_ready() {
        let m = ConversationModel::from_messages(&[], &[], SessionStatus::Running, false);
        assert!(!m.items.is_empty());
    }

    #[test]
    fn scroll_unpins_follow() {
        let mut m = ConversationModel::from_messages(&[], &[], SessionStatus::Running, false);
        assert!(m.follow);
        m.scroll_up(3);
        assert!(!m.follow);
        m.scroll = 0;
        m.scroll_down(1);
        assert!(m.follow);
    }
}
