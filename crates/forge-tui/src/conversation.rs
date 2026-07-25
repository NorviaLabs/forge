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

#[derive(Debug, Clone, PartialEq)]
pub enum ChatItem {
    /// Brand splash (replaces dumping system prompts into the chat).
    Brand,
    System {
        text: String,
    },
    User {
        text: String,
    },
    /// Model reasoning, shown in the conversation as muted text.
    Thinking {
        text: String,
        /// When set, thinking is finished and includes its elapsed-time summary.
        duration_secs: Option<f64>,
    },
    Assistant {
        text: String,
    },
    Queued {
        index: usize,
        text: String,
        selected: bool,
    },
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
        /// Brief operator-facing explanation for the change.
        rationale: String,
    },
    Banner {
        text: String,
        kind: BannerKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    Info,
    Warn,
    Error,
    Ok,
}

/// Live status while the model turn is in flight (before answer tokens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamWaitPhase {
    /// No tokens yet — waiting on the model.
    Waiting,
    /// Receiving thinking / reasoning tokens.
    Thinking,
}

/// Render options for progressive disclosure / density.
#[derive(Debug, Clone)]
pub struct ConversationViewOpts {
    pub busy: bool,
    /// Expand the last tool card's full output.
    pub tool_expanded: bool,
    /// Compact density (fewer blank lines, tighter wrap).
    pub compact: bool,
    /// Busy detail consumed by the bottom status bar.
    pub stream_wait: Option<(StreamWaitPhase, f64)>,
    /// When thinking just finished (answer streaming), show its elapsed time.
    pub stream_thought_secs: Option<f64>,
}

impl Default for ConversationViewOpts {
    fn default() -> Self {
        Self {
            busy: false,
            tool_expanded: false,
            compact: false,
            stream_wait: None,
            stream_thought_secs: None,
        }
    }
}

/// Format elapsed time in 0.1s increments through 5s, then whole seconds.
pub fn format_elapsed_tenths(secs: f64) -> String {
    let secs = secs.max(0.0);
    if secs < 5.0 {
        let tenths = (secs * 10.0).floor() / 10.0;
        format!("{tenths:.1}s")
    } else {
        format!("{}s", secs.floor() as u64)
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
        // System prompts and tool call cards stay out of the operator chat.
        let mut items: Vec<ChatItem> = Vec::new();
        let mut latest_thinking: Option<String> = None;
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
                            latest_thinking = Some(th.clone());
                            items.push(ChatItem::Thinking {
                                text: th.clone(),
                                duration_secs: m.thinking_duration_secs,
                            });
                        }
                    }
                    if !m.content.is_empty() {
                        items.push(ChatItem::Assistant {
                            text: m.content.clone(),
                        });
                    }
                }
                // Tool results are not shown as chat messages (keeps the transcript clean).
                MessageRole::Tool => {
                    let name = m.name.as_deref().unwrap_or("tool");
                    if looks_like_diff(&m.content)
                        || name.contains("write")
                        || name.contains("search_replace")
                        || name == "edit"
                        || name == "git"
                    {
                        items.push(ChatItem::DiffCard {
                            path: extract_path_hint(name, &m.content),
                            lines: m.content.lines().map(|s| s.to_string()).collect(),
                            rationale: change_rationale(latest_thinking.as_deref()),
                        });
                    }
                }
            }
        }
        for e in events {
            if e.kind == "hitl_wait" {
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
            // Live stream: duration filled in by app when thinking ends
            self.items.push(ChatItem::Thinking {
                text: thinking,
                duration_secs: self.opts.stream_thought_secs,
            });
        }
        // Only show the answer bubble once content tokens start (status line covers wait/think).
        if !text.is_empty() {
            let mut body = text;
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

    pub fn with_queued_messages(
        mut self,
        items: impl IntoIterator<Item = String>,
        selected: Option<usize>,
    ) -> Self {
        for (i, text) in items.into_iter().enumerate() {
            self.items.push(ChatItem::Queued {
                index: i,
                text,
                selected: selected == Some(i),
            });
        }
        self
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
        self.items
            .iter()
            .rposition(|i| matches!(i, ChatItem::ToolCard { .. } | ChatItem::DiffCard { .. }))
    }

    pub fn lines(&self) -> Vec<Line<'static>> {
        self.lines_for_width(if self.opts.compact { 88 } else { 100 })
    }

    /// Build display lines for the actual conversation viewport. Paragraph does
    /// not wrap styled lines itself, so wrapping follows the full pane width.
    fn lines_for_width(&self, available_width: usize) -> Vec<Line<'static>> {
        let width = available_width.max(4);
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
                        Span::styled("FORGE", theme::brand().add_modifier(Modifier::BOLD)),
                        Span::styled("  ·  coding agent", theme::dim()),
                    ]));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                // Legacy system items (rare): muted only, never dump full prompt
                ChatItem::System { text } => {
                    let short: String = text.chars().take(120).collect();
                    let more = if text.chars().count() > 120 {
                        "…"
                    } else {
                        ""
                    };
                    lines.push(Line::from(vec![
                        Span::styled("│ ", theme::dim()),
                        Span::styled(format!("{short}{more}"), theme::muted()),
                    ]));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                // User: plain text with no prompt marker or indent.
                ChatItem::User { text } => {
                    let parts = wrap(text, width);
                    for (i, l) in parts.into_iter().enumerate() {
                        if i == 0 {
                            lines.push(
                                Line::from(Span::styled("user", theme::info()))
                                    .style(theme::user_message()),
                            );
                        }
                        lines.push(
                            Line::from(vec![Span::styled(l, theme::text())])
                                .style(theme::user_message()),
                        );
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                // Thinking: show the live body while processing; hide completed thoughts.
                ChatItem::Thinking {
                    text,
                    duration_secs,
                } => {
                    if duration_secs.is_some() && !self.opts.busy {
                        if gap {
                            lines.push(Line::from(""));
                        }
                        continue;
                    }
                    // Providers sometimes wrap the entire reasoning summary
                    // in Markdown bold markers. Thinking already has its own
                    // visual treatment, so do not expose those delimiters.
                    let text = text.replace("**", "");
                    for l in wrap(&text, width.saturating_sub(3)) {
                        lines.push(
                            Line::from(vec![
                                Span::styled("⋯ ", theme::info().add_modifier(Modifier::BOLD)),
                                Span::styled(l, theme::muted().add_modifier(Modifier::ITALIC)),
                            ])
                            .style(theme::thinking_message()),
                        );
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                // Response: teal left bar — no "Forge"
                ChatItem::Assistant { text } => {
                    let parts = assistant_lines(text, width.saturating_sub(3));
                    let long_response = parts.len() > 3;
                    if long_response {
                        lines.push(horizontal_rule(width));
                    }
                    lines.push(
                        Line::from(Span::styled("assistant", theme::brand()))
                            .style(theme::assistant_message()),
                    );
                    for (i, line) in parts.into_iter().enumerate() {
                        let gutter = if i == 0 { "▍ " } else { "  " };
                        let mut spans = vec![Span::styled(
                            gutter,
                            theme::brand().add_modifier(Modifier::BOLD),
                        )];
                        spans.extend(line.spans);
                        lines.push(Line::from(spans).style(theme::assistant_message()));
                    }
                    if long_response {
                        lines.push(horizontal_rule(width));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ChatItem::Queued {
                    index,
                    text,
                    selected,
                } => {
                    let parts = wrap(text, width.saturating_sub(10));
                    for (i, l) in parts.iter().enumerate() {
                        let gutter = if i == 0 {
                            format!("○ {} ", index + 1)
                        } else {
                            "    ".to_string()
                        };
                        let style = if *selected {
                            theme::selected_row()
                        } else {
                            theme::muted()
                        };
                        lines.push(Line::from(vec![
                            Span::styled(gutter, theme::warn()),
                            Span::styled(l.clone(), style),
                        ]));
                    }
                    if *selected {
                        lines.push(Line::from(Span::styled(
                            "  queued · Ctrl+Backspace cancel",
                            theme::dim(),
                        )));
                    } else {
                        lines.push(Line::from(Span::styled("  queued", theme::dim())));
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
                            lines.push(Line::from(Span::styled(format!("  {l}"), theme::muted())));
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
                            lines.push(Line::from(Span::styled(format!("  {l}"), theme::muted())));
                        }
                        if is_last && detail.chars().count() > summary.chars().count() {
                            lines.push(Line::from(Span::styled("  Ctrl+O expand", theme::dim())));
                        }
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ChatItem::DiffCard {
                    path,
                    lines: dl,
                    rationale,
                } => {
                    lines.push(Line::from(vec![
                        Span::styled("Δ ", theme::brand()),
                        Span::styled(path.clone(), theme::text().add_modifier(Modifier::BOLD)),
                        Span::styled("  diff", theme::dim()),
                    ]));
                    if !rationale.is_empty() {
                        for l in wrap(rationale, width.saturating_sub(4)).into_iter().take(2) {
                            lines.push(Line::from(vec![
                                Span::styled("  ", theme::info()),
                                Span::styled(l, theme::muted().add_modifier(Modifier::ITALIC)),
                            ]));
                        }
                    }
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
        lines
    }
}

fn change_rationale(thinking: Option<&str>) -> String {
    let Some(text) = thinking else {
        return String::new();
    };
    let summary = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("**"))
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    if summary.is_empty() {
        String::new()
    } else {
        summary.chars().take(240).collect()
    }
}

fn horizontal_rule(width: usize) -> Line<'static> {
    Line::from(Span::styled("─".repeat(width), theme::dim()))
}

#[allow(dead_code)] // kept for optional tool/diff UI later
fn looks_like_diff(content: &str) -> bool {
    content.contains("\n+") && content.contains("\n-")
        || content.lines().any(|l| l.starts_with("@@ "))
        || content.starts_with("diff --git")
}

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

/// Render assistant Markdown without pulling a full Markdown parser into the
/// TUI. Fenced code gets token coloring; inline backtick sections get a code
/// color while ordinary prose keeps the normal conversation style.
fn assistant_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut language = String::new();
    let mut fenced = false;

    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if let Some(fence) = trimmed.strip_prefix("```") {
            if fenced {
                out.push(Line::from(Span::styled(
                    "  ```".to_string(),
                    theme::code_punctuation(),
                )));
                fenced = false;
                language.clear();
            } else {
                language = fence.trim().to_ascii_lowercase();
                let label = if language.is_empty() {
                    "  ```".to_string()
                } else {
                    format!("  ```{language}")
                };
                out.push(Line::from(Span::styled(label, theme::code_punctuation())));
                fenced = true;
            }
            continue;
        }

        if fenced {
            let chunks = if raw.is_empty() {
                vec![String::new()]
            } else {
                raw.chars()
                    .collect::<Vec<_>>()
                    .chunks(width)
                    .map(|chunk| chunk.iter().collect())
                    .collect()
            };
            for chunk in chunks {
                out.push(Line::from(highlight_code_line(&chunk, &language)));
            }
        } else {
            let wrapped = wrap(raw, width);
            for line in wrapped {
                out.push(Line::from(highlight_inline_code(&line)));
            }
        }
    }

    if out.is_empty() {
        out.push(Line::from(String::new()));
    }
    out
}

fn highlight_inline_code(line: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        if start > 0 {
            spans.push(Span::styled(rest[..start].to_string(), theme::text()));
        }
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else {
            spans.push(Span::styled(after.to_string(), theme::tool()));
            return spans;
        };
        spans.push(Span::styled(
            after[..end].to_string(),
            theme::tool().add_modifier(Modifier::BOLD),
        ));
        rest = &after[end + 1..];
    }
    if !rest.is_empty() {
        spans.push(Span::styled(rest.to_string(), theme::text()));
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), theme::text()));
    }
    spans
}

fn highlight_code_line(line: &str, language: &str) -> Vec<Span<'static>> {
    let keywords = match language {
        "rust" | "rs" => &[
            "as", "async", "await", "const", "crate", "else", "enum", "fn", "for", "if", "impl",
            "in", "let", "match", "mod", "move", "pub", "ref", "return", "self", "Self", "struct",
            "trait", "type", "use", "where", "while",
        ][..],
        "python" | "py" => &[
            "and", "as", "assert", "class", "def", "elif", "else", "False", "for", "from", "if",
            "import", "in", "is", "None", "not", "or", "pass", "return", "True", "while", "with",
        ][..],
        "javascript" | "js" | "typescript" | "ts" => &[
            "async",
            "await",
            "class",
            "const",
            "else",
            "export",
            "false",
            "for",
            "from",
            "function",
            "if",
            "import",
            "interface",
            "let",
            "new",
            "null",
            "return",
            "true",
            "type",
            "var",
            "while",
        ][..],
        "json" | "yaml" | "yml" => &["true", "false", "null"][..],
        "bash" | "sh" | "shell" => &[
            "case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if", "in",
            "then", "while",
        ][..],
        _ => &[
            "fn", "function", "if", "else", "for", "while", "return", "class", "const", "let",
            "true", "false", "null",
        ][..],
    };

    let chars: Vec<char> = line.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if (c == '/' && chars.get(i + 1) == Some(&'/'))
            || (c == '#' && matches!(language, "python" | "py" | "bash" | "sh" | "shell"))
            || (c == '-' && chars.get(i + 1) == Some(&'-'))
        {
            let comment: String = chars[i..].iter().collect();
            spans.push(Span::styled(comment, theme::code_comment()));
            break;
        }
        if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                let closed = chars[i] == quote;
                i += 1;
                if closed {
                    break;
                }
            }
            spans.push(Span::styled(
                chars[start..i.min(chars.len())].iter().collect::<String>(),
                theme::code_string(),
            ));
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            spans.push(Span::styled(
                chars[start..i].iter().collect::<String>(),
                theme::code_number(),
            ));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let style = if keywords.contains(&word.as_str()) {
                theme::code_keyword()
            } else {
                theme::text()
            };
            spans.push(Span::styled(word, style));
            continue;
        }
        spans.push(Span::styled(c.to_string(), theme::code_punctuation()));
        i += 1;
    }
    spans
}

pub struct ConversationWidget<'a> {
    pub model: &'a ConversationModel,
}

impl Widget for ConversationWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Account for the left and right borders. The model performs its own
        // styled wrapping, so it must use the viewport width rather than the
        // historical 100-column default.
        let lines = self
            .model
            .lines_for_width(area.width.saturating_sub(2) as usize);
        let total = lines.len() as u16;
        let height = area.height.saturating_sub(2);
        let max_scroll = total.saturating_sub(height);
        let scroll = if self.model.follow {
            max_scroll
        } else {
            // `model.scroll` is the distance from the bottom, so scrolling up
            // moves the viewport back from the live tail of the conversation.
            max_scroll.saturating_sub(self.model.scroll.min(max_scroll))
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border())
            .style(theme::panel_alt())
            .title(Span::styled(" transcript ", theme::muted()));
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
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                role: MessageRole::User,
                content: "hi".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                role: MessageRole::Assistant,
                content: "yo".into(),
                tool_call_id: None,
                name: None,
                thinking: Some("**ponder**".into()),
                thinking_duration_secs: Some(2.4),
                tool_calls: vec![],
            },
            Message {
                role: MessageRole::Tool,
                content: "ok body".into(),
                tool_call_id: Some("1".into()),
                name: Some("read_file".into()),
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
        ];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );
        // System + tool messages hidden; user/assistant/thinking only
        assert!(matches!(m.items[0], ChatItem::User { .. }));
        assert!(matches!(m.items[1], ChatItem::Thinking { .. }));
        assert!(matches!(m.items[2], ChatItem::Assistant { .. }));
        assert!(
            !m.items
                .iter()
                .any(|i| matches!(i, ChatItem::ToolCard { .. })),
            "tool cards should not appear in chat"
        );
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
        assert!(
            !rendered.contains("FORGE"),
            "brand splash removed from chat:\n{rendered}"
        );
        assert!(
            !rendered.to_ascii_lowercase().contains("read_file"),
            "tool call should not render:\n{rendered}"
        );
        assert!(
            !rendered.contains("Thought for"),
            "completed thought summary should be hidden:\n{rendered}"
        );
        assert!(
            !rendered.contains("ponder"),
            "completed thinking body should be hidden:\n{rendered}"
        );
        assert!(
            !rendered.contains("**"),
            "Markdown bold delimiters should not leak into thoughts:\n{rendered}"
        );
    }

    #[test]
    fn completed_thinking_is_hidden_in_lines() {
        let msgs = vec![Message {
            role: MessageRole::Assistant,
            content: "ans".into(),
            tool_call_id: None,
            name: None,
            thinking: Some("long thinking text here that should collapse".into()),
            thinking_duration_secs: Some(3.1),
            tool_calls: vec![],
        }];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            SessionStatus::Running,
            ConversationViewOpts {
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
            !text.contains("Thought for"),
            "completed thought summary should be hidden, got:\n{text}"
        );
        assert!(
            !text.contains("long thinking"),
            "completed thinking body should be hidden, got:\n{text}"
        );
    }

    #[test]
    fn wide_viewport_does_not_wrap_at_the_old_column_limit() {
        let content = std::iter::repeat("word")
            .take(24)
            .collect::<Vec<_>>()
            .join(" ");
        let msgs = vec![Message {
            role: MessageRole::Assistant,
            content,
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        }];
        let model = ConversationModel::from_messages(
            &msgs,
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );

        let answer_lines = model
            .lines_for_width(140)
            .iter()
            .filter(|line| {
                line.spans
                    .first()
                    .is_some_and(|span| span.content.as_ref() == "▍ ")
            })
            .count();
        assert_eq!(answer_lines, 1);
    }

    #[test]
    fn active_thinking_wraps_to_the_viewport_width() {
        let msgs = vec![Message {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_call_id: None,
            name: None,
            thinking: Some(
                "one two three four five six seven eight nine ten eleven twelve thirteen".into(),
            ),
            thinking_duration_secs: None,
            tool_calls: vec![],
        }];
        let model = ConversationModel::from_messages(
            &msgs,
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );

        let thought_lines = model
            .lines_for_width(24)
            .iter()
            .filter(|line| {
                line.spans
                    .first()
                    .is_some_and(|span| span.content.as_ref() == "⋯ ")
            })
            .count();
        assert_eq!(thought_lines, 4, "thinking must wrap at the pane width");
    }

    #[test]
    fn active_thinking_wraps_across_lines() {
        let msgs = vec![Message {
            role: MessageRole::Assistant,
            content: "ans".into(),
            tool_call_id: None,
            name: None,
            thinking: Some("this is a very long active thinking message that should wrap into multiple lines in the conversation pane".into()),
            thinking_duration_secs: None,
            tool_calls: vec![],
        }];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );
        let rendered_lines: Vec<String> = m
            .lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        let thought_lines = rendered_lines
            .iter()
            .filter(|line| line.starts_with("⋯ "))
            .count();
        assert!(
            thought_lines > 1,
            "active thinking should wrap to multiple lines, got:\n{}",
            rendered_lines.join("\n")
        );
    }

    #[test]
    fn user_messages_render_without_prompt_marker() {
        let msgs = vec![Message {
            role: MessageRole::User,
            content: "hello world".into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        }];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );
        let rendered = m
            .lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!rendered.contains('›'), "{rendered}");
        assert!(!rendered.contains("❯"), "{rendered}");
        assert!(rendered.contains("hello world"), "{rendered}");
    }

    #[test]
    fn empty_transcript_renders_without_initial_marker() {
        let model = ConversationModel::from_messages(
            &[],
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );
        let rendered = model
            .lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.is_empty(), "{rendered}");
        assert!(!rendered.contains('▸'), "{rendered}");
    }

    #[test]
    fn long_assistant_responses_get_horizontal_rules() {
        let msgs = vec![Message {
            role: MessageRole::Assistant,
            content: "line one\nline two\nline three\nline four".into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        }];
        let model = ConversationModel::from_messages(
            &msgs,
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );
        let rendered = model
            .lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(rendered.iter().filter(|line| line.contains('─')).count(), 2);
    }

    #[test]
    fn tool_messages_are_hidden() {
        let msgs = vec![Message {
            role: MessageRole::Tool,
            content: "Tool validation error: bad".into(),
            tool_call_id: Some("1".into()),
            name: Some("read_file".into()),
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        }];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );
        assert!(
            !m.items
                .iter()
                .any(|i| matches!(i, ChatItem::ToolCard { .. })),
            "tool messages must not become chat cards"
        );
        // Empty chat stays empty; the pane no longer seeds a placeholder row.
        assert!(m.items.is_empty());
    }

    #[test]
    fn diff_like_tool_messages_render_as_diff_cards() {
        let msgs = vec![Message {
            role: MessageRole::Tool,
            content: "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"
                .into(),
            tool_call_id: Some("1".into()),
            name: Some("write_file".into()),
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        }];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );
        assert!(matches!(m.items[0], ChatItem::DiffCard { .. }));
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
            rendered.contains("diff --git a/src/lib.rs b/src/lib.rs"),
            "{rendered}"
        );
        assert!(rendered.contains("+new"), "{rendered}");
        assert!(
            !rendered.contains("Applied by"),
            "code changes without reasoning should not show a generated rationale: {rendered}"
        );
        assert!(
            !rendered.contains("why:"),
            "the rationale should not have a why prefix: {rendered}"
        );
    }

    #[test]
    fn empty_shows_blank_conversation() {
        let m = ConversationModel::from_messages(
            &[],
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );
        assert!(m.items.is_empty());
        assert!(m.lines().is_empty());
    }

    #[test]
    fn elapsed_tenths_format() {
        assert_eq!(format_elapsed_tenths(0.0), "0.0s");
        assert_eq!(format_elapsed_tenths(0.14), "0.1s");
        assert_eq!(format_elapsed_tenths(1.29), "1.2s");
        assert_eq!(format_elapsed_tenths(4.99), "4.9s");
        assert_eq!(format_elapsed_tenths(5.0), "5s");
        assert_eq!(format_elapsed_tenths(5.99), "5s");
        assert_eq!(format_elapsed_tenths(12.99), "12s");
    }

    #[test]
    fn stream_wait_status_is_not_rendered_inline() {
        let mut m = ConversationModel::from_messages(
            &[],
            &[],
            SessionStatus::Running,
            ConversationViewOpts {
                busy: true,
                stream_wait: Some((StreamWaitPhase::Thinking, 1.2)),
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
        assert!(!text.contains("Thinking..."), "{text}");
        assert!(!text.contains("1.2s"), "{text}");

        m.opts.stream_wait = Some((StreamWaitPhase::Waiting, 0.3));
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
        assert!(!text.contains("Working..."), "{text}");
        assert!(!text.contains("0.3s"), "{text}");
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
