//! Conversation view model (TUI-02) — polished chat, thinking, tools, diffs.

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
    /// Model chain-of-thought / reasoning (muted; collapsible when done).
    Thinking { text: String },
    Assistant { text: String },
    ToolCard {
        name: String,
        summary: String,
        /// Full tool body for expand-on-demand.
        detail: String,
        state: ToolCardState,
        /// Optional duration label e.g. "142ms" (when known).
        duration: Option<String>,
    },
    /// Unified-ish diff snippet for write tools.
    DiffCard {
        path: String,
        lines: Vec<String>,
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

/// Render options for progressive disclosure / density.
#[derive(Debug, Clone)]
pub struct ConversationViewOpts {
    pub busy: bool,
    /// Expand completed thinking blocks (streaming thinking always expands).
    pub thinking_expanded: bool,
    /// Expand the last tool card's full output.
    pub tool_expanded: bool,
    /// Compact density (fewer blank lines, tighter wrap).
    pub compact: bool,
}

impl Default for ConversationViewOpts {
    fn default() -> Self {
        Self {
            busy: false,
            thinking_expanded: false,
            tool_expanded: false,
            compact: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConversationModel {
    pub items: Vec<ChatItem>,
    pub scroll: u16,
    pub follow: bool,
    pub opts: ConversationViewOpts,
}

impl ConversationModel {
    pub fn from_messages(
        messages: &[Message],
        events: &[TurnEvent],
        status: SessionStatus,
        opts: ConversationViewOpts,
    ) -> Self {
        // Brand banner instead of dumping model system prompts into the chat.
        let mut items = vec![ChatItem::Brand];
        for m in messages {
            match m.role {
                // System prompts are for the model, not the operator UI.
                MessageRole::System => {}
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
                MessageRole::Assistant => {
                    if let Some(ref th) = m.thinking {
                        if !th.trim().is_empty() {
                            items.push(ChatItem::Thinking {
                                text: th.clone(),
                            });
                        }
                    }
                    if !m.content.is_empty() {
                        items.push(ChatItem::Assistant {
                            text: m.content.clone(),
                        });
                    }
                }
                MessageRole::Tool => {
                    let name = m.name.clone().unwrap_or_else(|| "tool".into());
                    let (state, summary, detail) = classify_tool_content(&name, &m.content);
                    // Diff card for write-like tools when content looks like a patch
                    if looks_like_diff(&m.content) {
                        let path = extract_path_hint(&name, &m.content);
                        let lines = diff_preview_lines(&m.content, 24);
                        items.push(ChatItem::DiffCard { path, lines });
                    }
                    items.push(ChatItem::ToolCard {
                        name,
                        summary,
                        detail,
                        state,
                        duration: None,
                    });
                }
            }
        }
        for e in events {
            if e.kind == "context_reset" {
                items.push(ChatItem::Banner {
                    text: format!("Context handoff · {}", e.detail),
                    kind: BannerKind::Warn,
                });
            } else if e.kind == "hitl_wait" {
                items.push(ChatItem::Banner {
                    text: format!("Approval needed · {}", e.detail),
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
                text: "Awaiting approval · a approve · s allow session · d deny · Esc dismiss"
                    .into(),
                kind: BannerKind::Warn,
            });
        }
        // Only brand + no turns yet → short empty-state hint under the banner
        if items.len() == 1 {
            items.push(ChatItem::Banner {
                text: "Type a task · /connect · Ctrl+K".into(),
                kind: BannerKind::Info,
            });
        }
        Self {
            items,
            scroll: 0,
            follow: true,
            opts,
        }
    }

    pub fn from_session(session: &AgentSession, opts: ConversationViewOpts) -> Self {
        Self::from_messages(&session.messages, &session.events, session.status, opts)
    }

    /// Streaming thinking + assistant (thinking always expanded while busy).
    pub fn with_streaming_preview(
        mut self,
        thinking: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        let thinking = thinking.into();
        let text = text.into();
        if !thinking.is_empty() {
            self.items.push(ChatItem::Thinking { text: thinking });
        }
        if !text.is_empty() || self.opts.busy {
            let mut body = if text.is_empty() {
                "…".into()
            } else {
                text
            };
            if self.opts.busy && !body.ends_with('▌') {
                body.push('▌');
            }
            self.items.push(ChatItem::Assistant { text: body });
        }
        self
    }

    pub fn with_streaming_assistant(self, text: impl Into<String>) -> Self {
        self.with_streaming_preview("", text)
    }

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

    /// Index of the last tool card (for expand).
    pub fn last_tool_index(&self) -> Option<usize> {
        self.items.iter().rposition(|i| matches!(i, ChatItem::ToolCard { .. } | ChatItem::DiffCard { .. }))
    }

    pub fn lines(&self) -> Vec<Line<'static>> {
        let width: usize = if self.opts.compact { 88 } else { 100 };
        let gap = !self.opts.compact;
        let mut lines = Vec::new();
        let tool_count = self
            .items
            .iter()
            .filter(|i| matches!(i, ChatItem::ToolCard { .. }))
            .count();
        let mut tool_i = 0usize;
        let last_tool = self.last_tool_index();

        for (idx, item) in self.items.iter().enumerate() {
            match item {
                ChatItem::Brand => {
                    // Compact brand splash (not the model system prompt)
                    lines.push(Line::from(vec![
                        Span::styled("  ⬡  ", theme::brand()),
                        Span::styled(
                            "FORGE",
                            theme::brand().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("  ·  coding agent", theme::dim()),
                    ]));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                // Legacy system items (rare): muted only, never dump full prompt
                ChatItem::System { text } => {
                    let short: String = text.chars().take(120).collect();
                    let more = if text.chars().count() > 120 { "…" } else { "" };
                    lines.push(Line::from(vec![
                        Span::styled("│ ", theme::dim()),
                        Span::styled(format!("{short}{more}"), theme::muted()),
                    ]));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                // User: right-of-gutter › in info blue — no "You"
                ChatItem::User { text } => {
                    let parts = wrap(text, width.saturating_sub(3));
                    for (i, l) in parts.iter().enumerate() {
                        let gutter = if i == 0 { "› " } else { "  " };
                        lines.push(Line::from(vec![
                            Span::styled(
                                gutter,
                                theme::info().add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(l.clone(), theme::text()),
                        ]));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                // Thinking: dim italic · gutter — no "Thinking" word
                ChatItem::Thinking { text } => {
                    let streaming = self.opts.busy;
                    let expanded = streaming || self.opts.thinking_expanded;
                    if expanded {
                        for l in wrap(text, width.saturating_sub(3)) {
                            lines.push(Line::from(vec![
                                Span::styled("· ", theme::dim()),
                                Span::styled(
                                    l,
                                    theme::muted().add_modifier(Modifier::ITALIC),
                                ),
                            ]));
                        }
                        if !streaming {
                            lines.push(Line::from(Span::styled(
                                "  ⌄  Ctrl+T",
                                theme::dim(),
                            )));
                        }
                    } else {
                        let n = text.chars().count();
                        let preview: String = text.chars().take(56).collect();
                        let more = if n > 56 { "…" } else { "" };
                        lines.push(Line::from(vec![
                            Span::styled("· ", theme::dim()),
                            Span::styled(
                                format!("{preview}{more}"),
                                theme::muted().add_modifier(Modifier::ITALIC),
                            ),
                            Span::styled("  ⌃ Ctrl+T", theme::dim()),
                        ]));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                // Response: teal left bar — no "Forge"
                ChatItem::Assistant { text } => {
                    let parts = wrap(text, width.saturating_sub(3));
                    for (i, l) in parts.iter().enumerate() {
                        let gutter = if i == 0 { "▍ " } else { "  " };
                        lines.push(Line::from(vec![
                            Span::styled(
                                gutter,
                                theme::brand().add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(l.clone(), theme::text()),
                        ]));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ChatItem::ToolCard {
                    name,
                    summary,
                    detail,
                    state,
                    duration,
                } => {
                    tool_i += 1;
                    let (tag, st) = match state {
                        ToolCardState::Running => ("●", theme::info()),
                        ToolCardState::Done => ("✓", theme::ok()),
                        ToolCardState::Blocked => ("⏸", theme::warn()),
                        ToolCardState::Error => ("✗", theme::danger()),
                    };
                    let dur = duration
                        .as_ref()
                        .map(|d| format!(" · {d}"))
                        .unwrap_or_default();
                    let count = if tool_count > 1 {
                        format!(" {tool_i}/{tool_count}")
                    } else {
                        String::new()
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("{tag} "), st),
                        Span::styled(
                            format!("{name}{count}"),
                            theme::tool().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(dur, theme::dim()),
                    ]));
                    let is_last = Some(idx) == last_tool;
                    let expand = self.opts.tool_expanded && is_last;
                    if expand {
                        for l in detail.lines().take(40) {
                            lines.push(Line::from(Span::styled(
                                format!("  {l}"),
                                theme::muted(),
                            )));
                        }
                        if detail.lines().count() > 40 {
                            lines.push(Line::from(Span::styled(
                                "  … truncated · Ctrl+O collapse",
                                theme::dim(),
                            )));
                        } else {
                            lines.push(Line::from(Span::styled(
                                "  (Ctrl+O collapse)",
                                theme::dim(),
                            )));
                        }
                    } else {
                        for l in wrap(summary, width.saturating_sub(4)).into_iter().take(3) {
                            lines.push(Line::from(Span::styled(
                                format!("  {l}"),
                                theme::muted(),
                            )));
                        }
                        if is_last && detail.chars().count() > summary.chars().count() {
                            lines.push(Line::from(Span::styled(
                                "  Ctrl+O expand",
                                theme::dim(),
                            )));
                        }
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ChatItem::DiffCard { path, lines: dl } => {
                    lines.push(Line::from(vec![
                        Span::styled("Δ ", theme::brand()),
                        Span::styled(path.clone(), theme::text().add_modifier(Modifier::BOLD)),
                        Span::styled("  diff", theme::dim()),
                    ]));
                    for l in dl {
                        let style = if l.starts_with('+') && !l.starts_with("+++") {
                            theme::ok()
                        } else if l.starts_with('-') && !l.starts_with("---") {
                            theme::danger()
                        } else {
                            theme::muted()
                        };
                        lines.push(Line::from(Span::styled(format!("  {l}"), style)));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ChatItem::Banner { text, kind } => {
                    let st = match kind {
                        BannerKind::Info => theme::info(),
                        BannerKind::Warn => theme::warn(),
                        BannerKind::Error => theme::danger(),
                        BannerKind::Ok => theme::ok(),
                    };
                    for l in wrap(text, width) {
                        lines.push(Line::from(Span::styled(format!("▸ {l}"), st)));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
            }
        }
        if self.opts.busy {
            lines.push(Line::from(Span::styled(
                "  ⠋  Esc",
                theme::info().add_modifier(Modifier::ITALIC),
            )));
        }
        lines
    }
}

fn looks_like_diff(content: &str) -> bool {
    content.contains("\n+")
        && content.contains("\n-")
        || content.lines().any(|l| l.starts_with("@@ "))
        || content.starts_with("diff --git")
}

fn extract_path_hint(name: &str, content: &str) -> String {
    for line in content.lines().take(8) {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            return rest.trim().to_string();
        }
        if let Some(rest) = line.strip_prefix("--- a/") {
            return rest.trim().to_string();
        }
        if line.contains("path") && line.contains(':') {
            if let Some(p) = line.split('"').nth(1) {
                if p.contains('/') || p.contains('.') {
                    return p.to_string();
                }
            }
        }
    }
    name.to_string()
}

fn diff_preview_lines(content: &str, max: usize) -> Vec<String> {
    content
        .lines()
        .filter(|l| {
            l.starts_with('+')
                || l.starts_with('-')
                || l.starts_with("@@")
                || l.starts_with("diff ")
        })
        .take(max)
        .map(|s| s.chars().take(100).collect())
        .collect()
}

fn classify_tool_content(name: &str, content: &str) -> (ToolCardState, String, String) {
    let lower = content.to_ascii_lowercase();
    let state = if lower.contains("validation") || lower.contains("denied by acl") {
        ToolCardState::Error
    } else if lower.contains("hitl") || lower.contains("awaiting") {
        ToolCardState::Blocked
    } else {
        ToolCardState::Done
    };
    let detail = content.to_string();
    // One-line operator summary
    let first = content.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let summary = if name == "read_file" || name.contains("read") {
        let n = content.lines().count();
        format!("{first} · {n} lines")
    } else if name.contains("write") || name.contains("search_replace") || name == "edit" {
        format!("wrote · {}", first.chars().take(80).collect::<String>())
    } else if name == "git" {
        format!("{}", first.chars().take(100).collect::<String>())
    } else {
        content.chars().take(160).collect()
    };
    (state, summary, detail)
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
                content: "You are Forge, a coding agent. Use tools when needed.".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
            },
            Message {
                role: MessageRole::User,
                content: "hi".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
            },
            Message {
                role: MessageRole::Assistant,
                content: "yo".into(),
                tool_call_id: None,
                name: None,
                thinking: Some("ponder".into()),
            },
            Message {
                role: MessageRole::Tool,
                content: "ok body".into(),
                tool_call_id: Some("1".into()),
                name: Some("read_file".into()),
                thinking: None,
            },
        ];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );
        // System prompt is hidden; brand banner first
        assert!(matches!(m.items[0], ChatItem::Brand));
        assert!(matches!(m.items[1], ChatItem::User { .. }));
        assert!(matches!(m.items[2], ChatItem::Thinking { .. }));
        assert!(matches!(m.items[3], ChatItem::Assistant { .. }));
        assert!(matches!(
            m.items[4],
            ChatItem::ToolCard {
                state: ToolCardState::Done,
                ..
            }
        ));
        // Full system prompt must not appear in rendered lines
        let rendered: String = m
            .lines()
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !rendered.contains("You are Forge, a coding agent"),
            "system prompt leaked into UI:\n{rendered}"
        );
        assert!(rendered.contains("FORGE"), "expected brand banner");
    }

    #[test]
    fn thinking_collapses_in_lines() {
        let msgs = vec![Message {
            role: MessageRole::Assistant,
            content: "ans".into(),
            tool_call_id: None,
            name: None,
            thinking: Some("long thinking text here that should collapse".into()),
        }];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            SessionStatus::Running,
            ConversationViewOpts {
                thinking_expanded: false,
                ..Default::default()
            },
        );
        let text: String = m
            .lines()
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("Ctrl+T") || text.contains('⌃'),
            "expected collapse affordance, got:\n{text}"
        );
    }

    #[test]
    fn validation_is_error_card() {
        let msgs = vec![Message {
            role: MessageRole::Tool,
            content: "Tool validation error: bad".into(),
            tool_call_id: Some("1".into()),
            name: Some("read_file".into()),
            thinking: None,
        }];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );
        match &m.items[0] {
            ChatItem::ToolCard { state, .. } => assert_eq!(*state, ToolCardState::Error),
            _ => panic!("expected tool card"),
        }
    }

    #[test]
    fn empty_shows_brand() {
        let m = ConversationModel::from_messages(
            &[],
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );
        assert!(matches!(m.items[0], ChatItem::Brand));
    }

    #[test]
    fn scroll_unpins_follow() {
        let mut m = ConversationModel::from_messages(
            &[],
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );
        assert!(m.follow);
        m.scroll_up(3);
        assert!(!m.follow);
        m.scroll = 0;
        m.scroll_down(1);
        assert!(m.follow);
    }
}
