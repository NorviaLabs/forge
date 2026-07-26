//! Conversation view model (TUI-02) — polished chat, thinking, tools, diffs.

use crate::theme;
use forge_core::{AgentSession, TurnEvent};
use forge_syntax::highlight_to_lines;
use forge_types::{Message, MessageRole, SessionStatus};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

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
    Home {
        workspace: String,
        journal: String,
    },
    ContextHandoff {
        before_pct: f64,
        after_pct: f64,
        goal: String,
        completed: Vec<String>,
        next_actions: Vec<String>,
    },
    SessionRecovery {
        session_id: String,
        journal_path: String,
        last_seq: u64,
        model_steps: usize,
        tool_results: usize,
        incomplete_intents: usize,
        last_assistant: Option<String>,
    },
    SessionStatus {
        session_id: String,
        status: String,
        provider: String,
        model: String,
        context_tokens: usize,
        context_capacity: usize,
        context_pct: f64,
        reset_pct: f64,
        workspace: String,
        journal: String,
        cursor: u64,
        tools: usize,
        hitl_pending: bool,
    },
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
    RetryAssistant {
        text: String,
    },
    ValidationFailure {
        tool: String,
        error: String,
        retry: usize,
    },
    StreamingAssistant {
        text: String,
    },
    EvaluatorReport {
        text: String,
    },
    GeneratorRepair {
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
        let mut repair_pending = false;
        let mut validation_retry_pending = false;
        let mut validation_failures = std::collections::HashMap::<String, usize>::new();
        for m in messages {
            match m.role {
                // System prompts are for the model, not the operator UI.
                MessageRole::System => {}
                MessageRole::User => {
                    if m.content.starts_with("[REPAIR TASK") {
                        repair_pending = true;
                        items.push(ChatItem::EvaluatorReport {
                            text: m.content.clone(),
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
                        if repair_pending {
                            items.push(ChatItem::GeneratorRepair {
                                text: m.content.clone(),
                            });
                            repair_pending = false;
                        } else if validation_retry_pending {
                            items.push(ChatItem::RetryAssistant {
                                text: m.content.clone(),
                            });
                            validation_retry_pending = false;
                        } else {
                            items.push(ChatItem::Assistant {
                                text: m.content.clone(),
                            });
                        }
                    }
                }
                // Tool results are not shown as chat messages (keeps the transcript clean).
                MessageRole::Tool => {
                    let name = m.name.as_deref().unwrap_or("tool");
                    if m.content.starts_with("Tool validation error:") {
                        validation_retry_pending = true;
                        let retry = validation_failures.entry(name.to_string()).or_default();
                        *retry += 1;
                        items.push(ChatItem::ValidationFailure {
                            tool: name.to_string(),
                            error: m
                                .content
                                .trim_start_matches("Tool validation error: ")
                                .trim_end_matches(" Please correct arguments.")
                                .to_string(),
                            retry: *retry,
                        });
                    } else if looks_like_diff(&m.content)
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
                    } else {
                        let (state, summary, detail) = classify_tool_content(name, &m.content);
                        items.push(ChatItem::ToolCard {
                            name: name.to_string(),
                            summary,
                            detail,
                            state,
                            duration: None,
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
            self.items.push(ChatItem::StreamingAssistant { text: body });
        }
        self
    }

    pub fn with_streaming_assistant(self, text: impl Into<String>) -> Self {
        self.with_streaming_preview("", text)
    }

    pub fn with_home(mut self, workspace: String, journal: String) -> Self {
        if self.items.is_empty() {
            self.items.push(ChatItem::Home { workspace, journal });
        }
        self
    }

    pub fn with_running_tool(mut self, name: impl Into<String>) -> Self {
        self.items.push(ChatItem::ToolCard {
            name: name.into(),
            summary: "journal: tool_intent committed · awaiting result".into(),
            detail: String::new(),
            state: ToolCardState::Running,
            duration: None,
        });
        self
    }

    pub fn with_blocked_tool(
        mut self,
        name: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        self.items.push(ChatItem::ToolCard {
            name: name.into(),
            summary: summary.into(),
            detail: String::new(),
            state: ToolCardState::Blocked,
            duration: None,
        });
        self
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
                ChatItem::Home { workspace, journal } => {
                    lines.push(Line::from(Span::styled("SYSTEM", theme::dim())));
                    lines.push(Line::from(vec![
                        Span::styled("Forge ready ", theme::text()),
                        Span::styled("◆ ", theme::ok()),
                        Span::styled("workspace ", theme::dim()),
                        Span::styled(workspace.clone(), theme::muted()),
                    ]));
                    lines.push(Line::from(vec![
                        Span::styled("Loaded AGENTS.md ", theme::muted()),
                        Span::styled("· journal ", theme::dim()),
                        Span::styled(journal.clone(), theme::muted()),
                    ]));
                    lines.push(Line::from(Span::styled(
                        "Type a task, or / for commands.",
                        theme::muted(),
                    )));
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled("ASSISTANT", theme::dim())));
                    lines.push(Line::from(Span::styled(
                        "Waiting for your first message.",
                        theme::text(),
                    )));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ChatItem::ContextHandoff {
                    before_pct,
                    after_pct,
                    goal,
                    completed,
                    next_actions,
                } => {
                    lines.push(Line::from(vec![
                        Span::styled("CONTEXT LIFECYCLE", theme::brand()),
                        Span::styled(" · hard reset + handoff", theme::muted()),
                    ]));
                    for step in [
                        "✓ wrote .forge/progress.json",
                        "✓ journaled context_reset",
                        "✓ cleared active window",
                        "✓ rehydrated from progress + workspace",
                    ] {
                        lines.push(Line::from(Span::styled(step, theme::ok())));
                    }
                    lines.push(Line::from(Span::styled(
                        "Large tool payloads remain as offload URIs (CTX-01).",
                        theme::dim(),
                    )));
                    lines.push(Line::from(""));
                    lines.push(Line::from(vec![
                        Span::styled("ASSISTANT", theme::brand()),
                        Span::styled(" · POST-RESET", theme::info()),
                    ]));
                    lines.push(Line::from(Span::styled(
                        format!(
                            "Continuing from handoff · context {before_pct:.0}% → {after_pct:.0}%"
                        ),
                        theme::text(),
                    )));
                    lines.push(Line::from(vec![
                        Span::styled("goal: ", theme::dim()),
                        Span::styled(goal.clone(), theme::text()),
                    ]));
                    if let Some(done) = completed.last() {
                        lines.push(Line::from(vec![
                            Span::styled("done: ", theme::dim()),
                            Span::styled(done.clone(), theme::muted()),
                        ]));
                    }
                    if let Some(next) = next_actions.first() {
                        lines.push(Line::from(vec![
                            Span::styled("next: ", theme::dim()),
                            Span::styled(next.clone(), theme::muted()),
                        ]));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ChatItem::SessionRecovery {
                    session_id,
                    journal_path,
                    last_seq,
                    model_steps,
                    tool_results,
                    incomplete_intents,
                    last_assistant,
                } => {
                    lines.push(Line::from(Span::styled("RECOVERY", theme::brand())));
                    lines.push(Line::from(Span::styled(
                        format!("Resumed session {session_id} · journal replay"),
                        theme::text(),
                    )));
                    lines.push(Line::from(vec![
                        Span::styled("✓ opened ", theme::ok()),
                        Span::styled(journal_path.clone(), theme::muted()),
                    ]));
                    lines.push(Line::from(Span::styled(
                        format!("✓ replayed to cursor #{last_seq}"),
                        theme::ok(),
                    )));
                    lines.push(Line::from(Span::styled(
                        format!("✓ {model_steps} model steps restored from journal"),
                        theme::ok(),
                    )));
                    lines.push(Line::from(Span::styled(
                        format!("✓ {tool_results} tool results restored · no re-exec"),
                        theme::ok(),
                    )));
                    if *incomplete_intents > 0 {
                        lines.push(Line::from(Span::styled(
                            format!(
                                "⚠ {incomplete_intents} incomplete tool intents retained fail-safe"
                            ),
                            theme::warn(),
                        )));
                    }
                    lines.push(Line::from(Span::styled(
                        "Completed LLM and tool steps are never re-run.",
                        theme::dim(),
                    )));
                    if let Some(restored) = last_assistant {
                        lines.push(Line::from(""));
                        lines.push(Line::from(vec![
                            Span::styled("ASSISTANT", theme::brand()),
                            Span::styled(" · RESTORED", theme::info()),
                        ]));
                        for line in wrap(restored, width).into_iter().take(3) {
                            lines.push(Line::from(Span::styled(line, theme::text())));
                        }
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ChatItem::SessionStatus {
                    session_id,
                    status,
                    provider,
                    model,
                    context_tokens,
                    context_capacity,
                    context_pct,
                    reset_pct,
                    workspace,
                    journal,
                    cursor,
                    tools,
                    hitl_pending,
                } => {
                    lines.push(Line::from(Span::styled("STATUS", theme::brand())));
                    for (heading, rows) in [
                        (
                            "Session",
                            vec![
                                ("id", session_id.clone()),
                                ("status", status.clone()),
                                ("surface", "tui".into()),
                                ("journal", format!("{journal} · cursor #{cursor}")),
                            ],
                        ),
                        (
                            "Model",
                            vec![
                                ("provider", provider.clone()),
                                ("model", model.clone()),
                                ("switch", "config-only (/model)".into()),
                            ],
                        ),
                        (
                            "Context",
                            vec![
                                (
                                    "used",
                                    format!(
                                        "{context_tokens} / {context_capacity} ({context_pct:.0}%)"
                                    ),
                                ),
                                ("threshold", format!("reset at {reset_pct:.0}%")),
                            ],
                        ),
                        (
                            "Workspace",
                            vec![("root", workspace.clone()), ("worktree", "off".into())],
                        ),
                        (
                            "Governance",
                            vec![
                                ("tools", format!("{tools} allowed")),
                                (
                                    "hitl",
                                    if *hitl_pending {
                                        "approval pending".into()
                                    } else {
                                        "none pending".into()
                                    },
                                ),
                            ],
                        ),
                    ] {
                        lines.push(Line::from(""));
                        lines.push(Line::from(Span::styled(heading, theme::text())));
                        for (label, value) in rows {
                            lines.push(Line::from(vec![
                                Span::styled(format!("{label:<12}"), theme::dim()),
                                Span::styled(value, theme::muted()),
                            ]));
                        }
                    }
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
                // User turns are easy to scan, but deliberately not boxed in.
                ChatItem::User { text } => {
                    let parts = wrap(text, width.saturating_sub(2));
                    lines.push(Line::from(Span::styled("You", theme::metadata_style())));
                    for (i, l) in parts.into_iter().enumerate() {
                        let indent = if i == 0 { "› " } else { "  " };
                        lines.push(Line::from(vec![
                            Span::styled(indent, theme::metadata_style()),
                            Span::styled(l, theme::user_message_style()),
                        ]));
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
                        lines.push(Line::from(vec![
                            Span::styled("⋯ ", theme::metadata_style()),
                            Span::styled(l, theme::progress_style()),
                        ]));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                // Final answers are primary transcript content.
                ChatItem::Assistant { text } => {
                    let parts = assistant_lines(text, width.saturating_sub(3));
                    lines.push(Line::from(Span::styled("Forge", theme::brand())));
                    for (i, line) in parts.into_iter().enumerate() {
                        let gutter = if i == 0 { "▍ " } else { "  " };
                        let mut spans = vec![Span::styled(
                            gutter,
                            theme::metadata_style().add_modifier(Modifier::BOLD),
                        )];
                        spans.extend(line.spans);
                        lines.push(Line::from(spans).style(theme::assistant_answer_style()));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ChatItem::RetryAssistant { text } => {
                    lines.push(Line::from(vec![
                        Span::styled("ASSISTANT", theme::brand()),
                        Span::styled(" · RETRY", theme::warn()),
                    ]));
                    for line in assistant_lines(text, width.saturating_sub(3)) {
                        let mut spans = vec![Span::styled("▍ ", theme::brand())];
                        spans.extend(line.spans);
                        lines.push(Line::from(spans));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ChatItem::ValidationFailure { tool, error, retry } => {
                    lines.push(Line::from(vec![
                        Span::styled("TOOL · REJECTED", theme::danger()),
                        Span::styled("  TOOL CONTRACT", theme::dim()),
                    ]));
                    lines.push(Line::from(vec![
                        Span::styled(format!("{tool}  "), theme::text()),
                        Span::styled("invalid arguments · schema enforced", theme::danger()),
                    ]));
                    for line in wrap(error, width.saturating_sub(2)) {
                        lines.push(Line::from(Span::styled(line, theme::muted())));
                    }
                    lines.push(Line::from(Span::styled(
                        "Side effects not executed · validation failure journaled",
                        theme::ok(),
                    )));
                    lines.push(Line::from(Span::styled(
                        format!("↻ automatic validation retry {retry}/3 sent to model"),
                        theme::warn(),
                    )));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ChatItem::StreamingAssistant { text } => {
                    lines.push(Line::from(vec![
                        Span::styled("Forge", theme::brand()),
                        Span::styled(" · responding", theme::metadata_style()),
                    ]));
                    for (i, line) in assistant_lines(text, width.saturating_sub(3))
                        .into_iter()
                        .enumerate()
                    {
                        let gutter = if i == 0 { "▍ " } else { "  " };
                        let mut spans = vec![Span::styled(
                            gutter,
                            theme::metadata_style().add_modifier(Modifier::BOLD),
                        )];
                        spans.extend(line.spans);
                        lines.push(Line::from(spans).style(theme::assistant_answer_style()));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ChatItem::EvaluatorReport { text } => {
                    let body = text
                        .lines()
                        .skip_while(|line| line.trim_start().starts_with("[REPAIR TASK"));
                    lines.push(Line::from(vec![
                        Span::styled("FEEDBACK", theme::warn()),
                        Span::styled(" · EVAL-01", theme::dim()),
                    ]));
                    lines.push(Line::from(Span::styled(
                        "Dual-sensor gate after implementation step.",
                        theme::muted(),
                    )));
                    for line in body {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let style = if trimmed.starts_with("SENSOR")
                            || trimmed.starts_with("EVALUATOR REPORT")
                        {
                            theme::tool().add_modifier(Modifier::BOLD)
                        } else if trimmed.starts_with("Finding:") || trimmed.starts_with("Repair:")
                        {
                            theme::warn()
                        } else {
                            theme::text()
                        };
                        for wrapped in wrap(trimmed, width) {
                            lines.push(Line::from(Span::styled(wrapped, style)));
                        }
                    }
                    lines.push(Line::from(Span::styled(
                        "+ enqueued repair task for Generator",
                        theme::warn(),
                    )));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ChatItem::GeneratorRepair { text } => {
                    lines.push(Line::from(vec![
                        Span::styled("GENERATOR", theme::brand()),
                        Span::styled(" · REPAIR", theme::warn()),
                    ]));
                    for line in assistant_lines(text, width) {
                        lines.push(line.style(theme::assistant_message()));
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
                        ToolCardState::Running => ("◐", theme::tool_running_style()),
                        ToolCardState::Done => ("✓", theme::tool_success_style()),
                        ToolCardState::Blocked => ("⏸", theme::warn()),
                        ToolCardState::Error => ("✗", theme::tool_failure_style()),
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
                    let is_last = Some(idx) == last_tool;
                    let expand = self.opts.tool_expanded && is_last;
                    let has_more =
                        !detail.is_empty() && detail.chars().count() > summary.chars().count();
                    lines.push(Line::from(vec![
                        Span::styled(format!("{tag} "), st),
                        Span::styled(
                            format!("{name}{count}"),
                            theme::text().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("  ", theme::metadata_style()),
                        Span::styled(compact_summary(summary), theme::metadata_style()),
                        Span::styled(dur, theme::dim()),
                        Span::styled(
                            if has_more && !expand {
                                "  · Ctrl+O more"
                            } else {
                                ""
                            },
                            theme::metadata_style(),
                        ),
                    ]));
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
                    if let Some(lang) = lang_from_path(path) {
                        let syntax_theme = forge_syntax::HighlightTheme::default();
                        let code: String = dl
                            .iter()
                            .filter(|l| {
                                !l.starts_with("+++")
                                    && !l.starts_with("---")
                                    && !l.starts_with("diff ")
                            })
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join("\n");
                        let highlighted = highlight_to_lines(lang, &code, &syntax_theme);
                        let mut code_idx = 0;
                        for l in dl {
                            if l.starts_with("+++")
                                || l.starts_with("---")
                                || l.starts_with("diff ")
                            {
                                lines
                                    .push(Line::from(Span::styled(format!("  {l}"), theme::dim())));
                            } else {
                                let prefix = if l.starts_with('+') {
                                    "+"
                                } else if l.starts_with('-') {
                                    "-"
                                } else {
                                    " "
                                };
                                let diff_style = if l.starts_with('+') {
                                    theme::ok()
                                } else if l.starts_with('-') {
                                    theme::danger()
                                } else {
                                    theme::muted()
                                };
                                if code_idx < highlighted.len() {
                                    let mut spans =
                                        vec![Span::styled(format!("  {prefix}"), diff_style)];
                                    for (text, rgb, bold, italic) in &highlighted[code_idx] {
                                        let mut st = ratatui::style::Style::default()
                                            .fg(ratatui::style::Color::Rgb(rgb.0, rgb.1, rgb.2));
                                        if *bold {
                                            st = st.add_modifier(Modifier::BOLD);
                                        }
                                        if *italic {
                                            st = st.add_modifier(Modifier::ITALIC);
                                        }
                                        spans.push(Span::styled(text.clone(), st));
                                    }
                                    lines.push(Line::from(spans));
                                } else {
                                    lines.push(Line::from(Span::styled(
                                        format!("  {l}"),
                                        diff_style,
                                    )));
                                }
                                code_idx += 1;
                            }
                        }
                    } else {
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

/// Keep the default tool row useful without surfacing internal storage and
/// bookkeeping details. The complete, copyable output remains behind Ctrl+O.
fn compact_summary(summary: &str) -> String {
    let useful = summary
        .lines()
        .map(str::trim)
        .find(|line| {
            !line.is_empty()
                && !line.contains("offload")
                && !line.contains("sha256")
                && !line.contains("://")
                && !line.contains("token_storage")
        })
        .unwrap_or("completed");
    let mut result: String = useful.chars().take(96).collect();
    if useful.chars().count() > result.chars().count() {
        result.push('…');
    }
    result
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
    let detail = redact_tool_output(content);
    let lower = detail.to_ascii_lowercase();
    let state = if lower.contains("validation") || lower.contains("denied by acl") {
        ToolCardState::Error
    } else if lower.contains("hitl") || lower.contains("awaiting") {
        ToolCardState::Blocked
    } else {
        ToolCardState::Done
    };
    // One-line operator summary
    let first = detail.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let summary = if name == "read_file" || name.contains("read") {
        let n = detail.lines().count();
        format!("{first} · {n} lines")
    } else if name.contains("write") || name.contains("search_replace") || name == "edit" {
        format!("wrote · {}", first.chars().take(80).collect::<String>())
    } else if name == "git" {
        format!("{}", first.chars().take(100).collect::<String>())
    } else {
        detail.chars().take(160).collect()
    };
    (state, summary, detail)
}

fn redact_tool_output(content: &str) -> String {
    let lower = content.to_ascii_lowercase();
    if lower.contains("api_key")
        || lower.contains("bearer ")
        || lower.contains("sk-")
        || lower.contains("secret=")
    {
        "[redacted tool output]".into()
    } else {
        content.to_string()
    }
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
    let mut code_block_lines: Vec<String> = Vec::new();

    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if let Some(fence) = trimmed.strip_prefix("```") {
            if fenced {
                // Process accumulated code block with tree-sitter
                if !code_block_lines.is_empty() {
                    let code = code_block_lines.join("\n");
                    let theme = forge_syntax::HighlightTheme::default();
                    let highlighted = highlight_to_lines(&language, &code, &theme);
                    for line_segments in highlighted {
                        out.push(Line::from(render_highlighted_line(&line_segments)));
                    }
                    code_block_lines.clear();
                }
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
            code_block_lines.push(raw.to_string());
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

fn render_highlighted_line(segments: &[(String, (u8, u8, u8), bool, bool)]) -> Vec<Span<'static>> {
    segments
        .iter()
        .map(|(text, rgb, bold, italic)| {
            let mut style = ratatui::style::Style::default()
                .fg(ratatui::style::Color::Rgb(rgb.0, rgb.1, rgb.2));
            if *bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if *italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
            Span::styled(text.clone(), style)
        })
        .collect()
}

pub struct ConversationWidget<'a> {
    pub model: &'a ConversationModel,
}

impl Widget for ConversationWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // The transcript owns the main area; hierarchy comes from spacing and
        // semantic markers rather than a permanent frame.
        let lines = self.model.lines_for_width(area.width as usize);
        let total = lines.len() as u16;
        let height = area.height;
        let max_scroll = total.saturating_sub(height);
        let scroll = if self.model.follow {
            max_scroll
        } else {
            // `model.scroll` is the distance from the bottom, so scrolling up
            // moves the viewport back from the live tail of the conversation.
            max_scroll.saturating_sub(self.model.scroll.min(max_scroll))
        };
        Paragraph::new(lines).scroll((scroll, 0)).render(area, buf);
    }
}

/// Detect language from file path, returning language name for syntax highlighting.
fn lang_from_path(path: &str) -> Option<&'static str> {
    let path_lower = path.to_lowercase();
    let filename = path_lower
        .rsplit('/')
        .next()
        .unwrap_or(&path_lower)
        .rsplit('\\')
        .next()
        .unwrap_or(&path_lower);

    let ext = filename.rsplit('.').next()?;
    match ext {
        "rs" => Some("rust"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" | "mjs" => Some("javascript"),
        "py" => Some("python"),
        "go" => Some("go"),
        "json" => Some("json"),
        "html" | "htm" => Some("html"),
        "css" => Some("css"),
        "sh" | "bash" | "zsh" => Some("bash"),
        "md" => Some("markdown"),
        "toml" | "yaml" | "yml" => Some("yaml"),
        "txt" | "log" => None,
        _ => None,
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
        // System prompts stay hidden while tool results become compact cards.
        assert!(matches!(m.items[0], ChatItem::User { .. }));
        assert!(matches!(m.items[1], ChatItem::Thinking { .. }));
        assert!(matches!(m.items[2], ChatItem::Assistant { .. }));
        assert!(m
            .items
            .iter()
            .any(|i| matches!(i, ChatItem::ToolCard { .. })));
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
            rendered.to_ascii_lowercase().contains("read_file"),
            "tool result card missing:\n{rendered}"
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
        assert!(rendered.contains("You\n› hello world"), "{rendered}");
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
    fn long_assistant_responses_use_a_single_semantic_heading() {
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
        assert_eq!(
            rendered
                .iter()
                .filter(|line| line.as_str() == "Forge")
                .count(),
            1
        );
        assert_eq!(rendered.iter().filter(|line| line.contains('─')).count(), 0);
    }

    #[test]
    fn tool_messages_render_as_cards() {
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
        assert!(matches!(
            m.items.first(),
            Some(ChatItem::ValidationFailure { tool, .. }) if tool == "read_file"
        ));
    }

    #[test]
    fn validation_failure_is_deduplicated_and_labels_retry() {
        let error = "Tool validation error: tool `read_file` validation failed at /path: 1 is not of type string. Please correct arguments.";
        let msgs = vec![
            Message {
                role: MessageRole::Tool,
                content: error.into(),
                tool_call_id: Some("1".into()),
                name: Some("read_file".into()),
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                role: MessageRole::Tool,
                content: error.into(),
                tool_call_id: Some("2".into()),
                name: Some("read_file".into()),
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                role: MessageRole::Assistant,
                content: "Correcting the tool call.".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
        ];
        let events = vec![TurnEvent {
            kind: "validation".into(),
            detail: error.into(),
        }];
        let model = ConversationModel::from_messages(
            &msgs,
            &events,
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );
        assert_eq!(model.items.len(), 3);
        assert!(matches!(model.items[0], ChatItem::ValidationFailure { .. }));
        assert!(matches!(
            model.items[1],
            ChatItem::ValidationFailure { retry: 2, .. }
        ));
        assert!(matches!(model.items[2], ChatItem::RetryAssistant { .. }));
        let rendered = model
            .lines()
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<String>();
        for expected in [
            "TOOL · REJECTED",
            "Side effects not executed",
            "retry 2/3",
            "ASSISTANT · RETRY",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected}: {rendered}"
            );
        }
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
    fn streaming_assistant_has_live_label_and_cursor() {
        let m = ConversationModel::from_messages(
            &[],
            &[],
            SessionStatus::Running,
            ConversationViewOpts {
                busy: true,
                ..Default::default()
            },
        )
        .with_streaming_assistant("partial response");
        let text = m
            .lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("Forge · responding"));
        assert!(text.contains("partial response▌"));
    }

    #[test]
    fn running_tool_card_shows_intent_without_arguments() {
        let m = ConversationModel::from_messages(
            &[],
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        )
        .with_running_tool("read_file");
        let text = m
            .lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("◐ read_file"));
        assert!(text.contains("tool_intent committed · awaiting result"));
    }

    #[test]
    fn blocked_tool_card_shows_redacted_summary() {
        let m = ConversationModel::from_messages(
            &[],
            &[],
            SessionStatus::AwaitingHitl,
            ConversationViewOpts::default(),
        )
        .with_blocked_tool("bash", "git push -u origin feature");
        let text = m
            .lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("⏸ bash"));
        assert!(text.contains("git push -u origin feature"));
    }

    #[test]
    fn context_handoff_card_shows_lifecycle_and_progress() {
        let m = ConversationModel {
            items: vec![ChatItem::ContextHandoff {
                before_pct: 82.0,
                after_pct: 14.0,
                goal: "rate limiting middleware".into(),
                completed: vec!["middleware scaffold".into()],
                next_actions: vec!["wire public router".into()],
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let text = m
            .lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        for expected in [
            "CONTEXT LIFECYCLE",
            "POST-RESET",
            "82% → 14%",
            "rate limiting middleware",
            "wire public router",
        ] {
            assert!(text.contains(expected), "missing {expected:?}: {text}");
        }
    }

    #[test]
    fn session_recovery_card_shows_replay_guarantees() {
        let m = ConversationModel {
            items: vec![ChatItem::SessionRecovery {
                session_id: "a1b2c3d4".into(),
                journal_path: ".forge/sessions/a1b2c3d4.db".into(),
                last_seq: 1847,
                model_steps: 62,
                tool_results: 41,
                incomplete_intents: 1,
                last_assistant: Some("Continuing from the restored journal.".into()),
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let text = m
            .lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        for expected in [
            "RECOVERY",
            "cursor #1847",
            "62 model steps",
            "41 tool results",
            "1 incomplete tool intents",
            "ASSISTANT · RESTORED",
            "never re-run",
        ] {
            assert!(text.contains(expected), "missing {expected:?}: {text}");
        }
    }

    #[test]
    fn repair_task_renders_evaluator_report_and_generator_response() {
        let messages = vec![
            Message {
                role: MessageRole::User,
                content: "[REPAIR TASK EVAL-01]\nSENSOR · DETERMINISTIC\ncargo test · failed\nEVALUATOR REPORT\nCriteria: public API returns 429\nFinding: layer is registered too late\nRepair: attach layer to public router".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                role: MessageRole::Assistant,
                content: "Moving the layer onto the public router.".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
        ];
        let model = ConversationModel::from_messages(
            &messages,
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );
        let text = model
            .lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        for expected in [
            "FEEDBACK · EVAL-01",
            "SENSOR · DETERMINISTIC",
            "EVALUATOR REPORT",
            "Finding:",
            "enqueued repair task for Generator",
            "GENERATOR · REPAIR",
        ] {
            assert!(text.contains(expected), "missing {expected:?}: {text}");
        }
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
