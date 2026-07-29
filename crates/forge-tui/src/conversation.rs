//! Conversation view model (TUI-02) — polished chat, thinking, tools, diffs.

use crate::theme;
use forge_core::{AgentSession, TurnEvent};
use forge_syntax::highlight_to_lines;
use forge_types::{Message, MessageRole, SessionStatus, ToolCall};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityCategory {
    Exploring,
    Implementing,
    Validating,
    Reviewing,
    Recovering,
    Waiting,
}

impl ActivityCategory {
    fn label(self, running: bool) -> &'static str {
        match (self, running) {
            (Self::Exploring, true) => "Exploring repository",
            (Self::Exploring, false) => "Explored repository",
            (Self::Implementing, true) => "Implementing changes",
            (Self::Implementing, false) => "Implemented changes",
            (Self::Validating, true) => "Running validation",
            (Self::Validating, false) => "Validation completed",
            (Self::Reviewing, true) => "Reviewing workspace",
            (Self::Reviewing, false) => "Reviewed workspace",
            (Self::Recovering, true) => "Recovering session",
            (Self::Recovering, false) => "Recovered session",
            (Self::Waiting, true) => "Waiting",
            (Self::Waiting, false) => "Waited",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChatItem {
    /// Brand splash (replaces dumping system prompts into the chat).
    Brand {
        version: String,
    },
    Home {
        workspace: String,
        skills_loaded: usize,
    },
    ActivitySummary {
        label: String,
        action: Option<String>,
        kind: BannerKind,
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
    ActivityGroup {
        category: ActivityCategory,
        summary: String,
        detail: String,
        state: ToolCardState,
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
#[derive(Debug, Clone, Default)]
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
        _events: &[TurnEvent],
        status: SessionStatus,
        opts: ConversationViewOpts,
    ) -> Self {
        // System prompts and tool call cards stay out of the operator chat.
        let mut items: Vec<ChatItem> = Vec::new();
        let tool_calls = messages
            .iter()
            .flat_map(|message| message.tool_calls.iter())
            .map(|call| (call.id.as_str(), call))
            .collect::<std::collections::HashMap<_, _>>();
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
                        let (clean_text, attachment_line) = strip_attached_context(&m.content);
                        items.push(ChatItem::User { text: clean_text });
                        if let Some(line) = attachment_line {
                            items.push(ChatItem::System { text: line });
                        }
                    }
                }
                MessageRole::Assistant => {
                    if let Some(ref th) = m.thinking {
                        if !th.trim().is_empty() {
                            latest_thinking = Some(th.clone());
                            // Reasoning is kept for diff rationale and model context but is not
                            // rendered as visible Chat rows by default.
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
                        || looks_like_code_change(name, &m.content)
                    {
                        let rationale = change_rationale(latest_thinking.as_deref());
                        for (path, lines) in split_diff_sections(name, &m.content) {
                            items.push(ChatItem::DiffCard {
                                path,
                                lines,
                                rationale: rationale.clone(),
                            });
                        }
                    } else {
                        let call = m
                            .tool_call_id
                            .as_deref()
                            .and_then(|id| tool_calls.get(id).copied());
                        let (state, summary, detail) =
                            classify_tool_content(name, &m.content, call);
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
        items = group_routine_activity(items);
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

    /// Streaming assistant preview. Live reasoning is tracked by the caller via
    /// `opts.stream_wait` / `opts.stream_thought_secs` and is not rendered as Chat rows.
    pub fn with_streaming_preview(
        mut self,
        _thinking: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        let text = text.into();
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

    pub fn with_brand(mut self, version: impl Into<String>) -> Self {
        if !self
            .items
            .iter()
            .any(|item| matches!(item, ChatItem::Brand { .. }))
        {
            self.items.insert(
                0,
                ChatItem::Brand {
                    version: version.into(),
                },
            );
        }
        self
    }

    pub fn with_home(mut self, workspace: String, skills_loaded: usize) -> Self {
        if !self
            .items
            .iter()
            .any(|item| matches!(item, ChatItem::Home { .. }))
        {
            let index = self
                .items
                .iter()
                .position(|item| matches!(item, ChatItem::Brand { .. }))
                .map(|idx| idx + 1)
                .unwrap_or(0);
            self.items.insert(
                index,
                ChatItem::Home {
                    workspace,
                    skills_loaded,
                },
            );
        }
        self
    }

    pub fn with_activity_summary(
        mut self,
        label: impl Into<String>,
        action: Option<impl Into<String>>,
        kind: BannerKind,
    ) -> Self {
        self.items
            .retain(|item| !matches!(item, ChatItem::ActivitySummary { .. }));
        let index = self
            .items
            .iter()
            .rposition(|item| matches!(item, ChatItem::Home { .. } | ChatItem::Brand { .. }))
            .map(|idx| idx + 1)
            .unwrap_or(0);
        self.items.insert(
            index,
            ChatItem::ActivitySummary {
                label: label.into(),
                action: action.map(Into::into),
                kind,
            },
        );
        self
    }

    pub fn with_running_tool(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        if let Some(category) = routine_tool_category(&name, "", None) {
            self.items.push(ChatItem::ActivityGroup {
                category,
                summary: running_activity_summary(category, &name),
                detail: format!("{name}: tool_intent committed · awaiting result"),
                state: ToolCardState::Running,
            });
            return self;
        }
        self.items.push(ChatItem::ToolCard {
            name,
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
        self.items.iter().rposition(|i| {
            matches!(
                i,
                ChatItem::ToolCard { .. }
                    | ChatItem::ActivityGroup { .. }
                    | ChatItem::DiffCard { .. }
            )
        })
    }

    pub fn lines(&self) -> Vec<Line<'static>> {
        self.lines_for_width(if self.opts.compact { 88 } else { 100 })
    }

    /// Build display lines for the actual conversation viewport. Paragraph does
    /// not wrap styled lines itself, so wrapping follows the full pane width.
    pub(crate) fn lines_for_width(&self, available_width: usize) -> Vec<Line<'static>> {
        let width = available_width.max(4);
        let gap = !self.opts.compact;
        let mut lines = Vec::new();
        let tool_count = self
            .items
            .iter()
            .filter(|i| {
                matches!(
                    i,
                    ChatItem::ToolCard { .. } | ChatItem::ActivityGroup { .. }
                )
            })
            .count();
        let mut tool_i = 0usize;
        let last_tool = self.last_tool_index();

        for (idx, item) in self.items.iter().enumerate() {
            match item {
                ChatItem::Brand { version } => {
                    let art = [
                        "  ███████╗ ██████╗ ██████╗  ██████╗ ███████╗",
                        "██╔════╝██╔═══██╗██╔══██╗██╔════╝ ██╔════╝",
                        "█████╗  ██║   ██║██████╔╝██║  ███╗█████╗",
                        "██╔══╝  ██║   ██║██╔══██╗██║   ██║██╔══╝",
                        "██║     ╚██████╔╝██║  ██║╚██████╔╝███████╗",
                        "╚═╝      ╚═════╝ ╚═╝  ╚═╝ ╚═════╝ ╚══════╝",
                    ];
                    let version_tag = format!("{version:>12}");
                    lines.push(Line::from(""));
                    for row in art {
                        lines.push(Line::from(centered_span(row, width, theme::warn())));
                    }
                    lines.push(Line::from(centered_span(
                        &version_tag,
                        width,
                        theme::warn().add_modifier(Modifier::BOLD),
                    )));
                    lines.push(Line::from(""));
                }
                ChatItem::Home { skills_loaded, .. } => {
                    let label = format!("Loaded AGENTS.md  ·  {skills_loaded} skills");
                    lines.push(Line::from(centered_span(&label, width, theme::muted())));
                    lines.push(Line::from(centered_span(
                        "Type a task, or / for commands.",
                        width,
                        theme::muted(),
                    )));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ChatItem::ActivitySummary {
                    label,
                    action,
                    kind,
                } => {
                    let st = match kind {
                        BannerKind::Info => theme::info(),
                        BannerKind::Warn => theme::warn(),
                        BannerKind::Error => theme::danger(),
                        BannerKind::Ok => theme::ok(),
                    };
                    let mut spans = vec![
                        Span::styled("● ", st),
                        Span::styled(label.clone(), theme::text().add_modifier(Modifier::BOLD)),
                    ];
                    if let Some(action) = action {
                        spans.push(Span::styled(" · ", theme::muted()));
                        spans.push(Span::styled(action.clone(), theme::brand()));
                    }
                    lines.push(Line::from(spans));
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
                    lines.push(Line::from(Span::styled(
                        "Continuing in a fresh context",
                        theme::brand(),
                    )));
                    lines.push(Line::from(Span::styled(
                        format!("Preserving task state · {before_pct:.0}% → {after_pct:.0}%"),
                        theme::muted(),
                    )));
                    if !goal.trim().is_empty() {
                        lines.push(kv_line("Objective", goal));
                    }
                    if !completed.is_empty() {
                        lines.push(kv_line("Completed", &completed.join(", ")));
                    }
                    if !next_actions.is_empty() {
                        lines.push(kv_line("Next action", &next_actions.join(", ")));
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
                    lines.push(Line::from(Span::styled(
                        "Restoring the previous session",
                        theme::brand(),
                    )));
                    lines.push(Line::from(Span::styled(
                        "Checking repository state before continuing.",
                        theme::muted(),
                    )));
                    lines.push(kv_line("Conversation", "restored"));
                    lines.push(kv_line("Tool results", &tool_results.to_string()));
                    if *incomplete_intents > 0 {
                        lines.push(kv_line(
                            "Interrupted work",
                            &format!("{incomplete_intents} item not repeated"),
                        ));
                    }
                    lines.push(kv_line(
                        "Runtime details",
                        &format!(
                            "session {session_id} · {journal_path} · seq {last_seq} · {model_steps} model steps"
                        ),
                    ));
                    let _ = last_assistant;
                    if let Some(restored) = last_assistant {
                        lines.push(Line::from(""));
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
                            "Work",
                            vec![
                                ("state", status.clone()),
                                ("repository", workspace.clone()),
                                ("validation", "Not available".into()),
                            ],
                        ),
                        (
                            "Model",
                            vec![
                                ("provider", provider.clone()),
                                ("model", model.clone()),
                                (
                                    "switch",
                                    "Applies to future turns. Current task state is preserved."
                                        .into(),
                                ),
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
                            "Runtime",
                            vec![
                                ("session", session_id.clone()),
                                ("journal", format!("{journal} · cursor #{cursor}")),
                                ("surface", "tui".into()),
                                ("tools", format!("{tools} allowed")),
                                (
                                    "approval",
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
                    for (i, l) in parts.into_iter().enumerate() {
                        let indent = if i == 0 { "› " } else { "  " };
                        lines.push(
                            Line::from(vec![
                                Span::styled(indent, theme::metadata_style()),
                                Span::styled(l, theme::user_message_style()),
                            ])
                            .style(theme::user_message()),
                        );
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                // Reasoning / chain-of-thought is intentionally not rendered as ordinary
                // Chat rows. It remains available for diff rationale and model context.
                ChatItem::Thinking { .. } => {
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                // Final answers are primary transcript content.
                ChatItem::Assistant { text } => {
                    let parts = assistant_lines(text, width.saturating_sub(3));
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
                    lines.push(Line::from(Span::styled(
                        "Retrying",
                        theme::progress_style(),
                    )));
                    for line in assistant_lines(text, width.saturating_sub(3)) {
                        let mut spans = vec![Span::styled("▍ ", theme::metadata_style())];
                        spans.extend(line.spans);
                        lines.push(Line::from(spans).style(theme::assistant_answer_style()));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ChatItem::ValidationFailure { tool, error, retry } => {
                    lines.push(Line::from(vec![
                        Span::styled("Correcting an invalid tool request", theme::danger()),
                        Span::styled("  DETAILS", theme::dim()),
                    ]));
                    lines.push(Line::from(vec![
                        Span::styled(format!("{tool}  "), theme::text()),
                        Span::styled("invalid arguments", theme::danger()),
                    ]));
                    for line in wrap(error, width.saturating_sub(2)) {
                        lines.push(Line::from(Span::styled(line, theme::muted())));
                    }
                    lines.push(Line::from(Span::styled(
                        "No command was executed and no files were changed.",
                        theme::ok(),
                    )));
                    lines.push(Line::from(Span::styled(
                        format!("Retrying with corrected tool details {retry}/3"),
                        theme::warn(),
                    )));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ChatItem::StreamingAssistant { text } => {
                    lines.push(Line::from(Span::styled(
                        "Responding",
                        theme::metadata_style(),
                    )));
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
                    lines.push(Line::from(Span::styled("Review", theme::progress_style())));
                    lines.push(Line::from(Span::styled(
                        "Findings, evidence, severity, and suggested actions.",
                        theme::progress_style(),
                    )));
                    for line in body {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let display = trimmed
                            .replace("EVALUATOR REPORT", "Review")
                            .replace("Evaluator Report", "Review")
                            .replace("Repair:", "Suggested action:")
                            .replace("Finding:", "Finding:");
                        let style = if trimmed.starts_with("SENSOR")
                            || trimmed.starts_with("EVALUATOR REPORT")
                        {
                            theme::tool()
                                .add_modifier(Modifier::BOLD)
                                .add_modifier(Modifier::ITALIC)
                        } else if trimmed.starts_with("Finding:") || trimmed.starts_with("Repair:")
                        {
                            theme::warn().add_modifier(Modifier::ITALIC)
                        } else {
                            theme::progress_style()
                        };
                        for wrapped in wrap(&display, width) {
                            lines.push(Line::from(Span::styled(wrapped, style)));
                        }
                    }
                    lines.push(Line::from(Span::styled(
                        "+ suggested action sent back to the task",
                        theme::warn().add_modifier(Modifier::ITALIC),
                    )));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ChatItem::GeneratorRepair { text } => {
                    lines.push(Line::from(Span::styled(
                        "Repairing",
                        theme::progress_style(),
                    )));
                    for line in assistant_lines(text, width) {
                        lines.push(line.style(theme::assistant_answer_style()));
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
                    let has_more = !detail.is_empty() && detail.trim() != summary.trim();
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
                ChatItem::ActivityGroup {
                    category,
                    summary,
                    detail,
                    state,
                } => {
                    tool_i += 1;
                    let (tag, st) = match state {
                        ToolCardState::Running => ("●", theme::tool_running_style()),
                        ToolCardState::Done => ("✓", theme::tool_success_style()),
                        ToolCardState::Blocked => ("⏸", theme::warn()),
                        ToolCardState::Error => ("✗", theme::tool_failure_style()),
                    };
                    let running = *state == ToolCardState::Running;
                    let is_last = Some(idx) == last_tool;
                    let expand = self.opts.tool_expanded && is_last;
                    lines.push(Line::from(vec![
                        Span::styled(format!("{tag} "), st),
                        Span::styled(
                            category.label(running),
                            theme::text().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("  ", theme::metadata_style()),
                        Span::styled(summary.clone(), theme::metadata_style()),
                        Span::styled(
                            if !expand { "  · Ctrl+O details" } else { "" },
                            theme::metadata_style(),
                        ),
                    ]));
                    if expand {
                        lines.push(Line::from(Span::styled(
                            "  Details are also available in Activity.",
                            theme::dim(),
                        )));
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
                    let (tag, st) = ("✓", theme::tool_success_style());
                    lines.push(Line::from(vec![
                        Span::styled(format!("{tag} "), st),
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
                    lines.extend(render_numbered_diff(path, dl, width));
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

/// If the message content begins with an Active-file context block, strip it
/// and return the clean user query plus a compact attachment summary.
fn strip_attached_context(content: &str) -> (String, Option<String>) {
    if !content.starts_with("Active file:") {
        return (content.to_string(), None);
    }

    // Find the first blank line followed by user content.
    if let Some(dbl_newline) = content.find("\n\n\n") {
        let header = &content[..dbl_newline];
        let user_text = content[dbl_newline + 3..].trim_start().to_string();

        // Extract the rel_path and cursor line from the header.
        let rel_path = header
            .lines()
            .find_map(|line| line.strip_prefix("Active file: "))
            .unwrap_or("")
            .to_string();
        let cursor = header
            .lines()
            .find_map(|line| line.strip_prefix("Cursor line: "))
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(0);

        let summary = format!("Attached: {rel_path}:{cursor}");
        (user_text, Some(summary))
    } else {
        (content.to_string(), None)
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

fn centered_span(text: &str, width: usize, style: ratatui::style::Style) -> Span<'static> {
    let text_width = text.chars().count();
    let pad = width.saturating_sub(text_width) / 2;
    Span::styled(format!("{:pad$}{text}", "", pad = pad), style)
}

fn kv_line(label: &'static str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}: "), theme::dim()),
        Span::styled(value.to_string(), theme::text()),
    ])
}

fn looks_like_diff(content: &str) -> bool {
    let has_hunk = content.lines().any(|line| {
        line.strip_prefix("@@ -")
            .and_then(|line| line.split_once(" +"))
            .is_some()
    });
    has_hunk
        && (content.starts_with("diff --git")
            || (content.lines().any(|line| line.starts_with("--- "))
                && content.lines().any(|line| line.starts_with("+++ "))))
}

fn looks_like_code_change(name: &str, content: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "write_file" | "apply_patch" | "search_replace" | "edit"
    ) && looks_like_diff(content)
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

fn split_diff_sections(name: &str, content: &str) -> Vec<(String, Vec<String>)> {
    let mut sections = Vec::new();
    let mut current = Vec::new();

    for line in content.lines() {
        if line.starts_with("diff --git ") && !current.is_empty() {
            while current.last().is_some_and(String::is_empty) {
                current.pop();
            }
            let body = current.join("\n");
            sections.push((extract_path_hint(name, &body), std::mem::take(&mut current)));
        }
        current.push(line.to_string());
    }

    if !current.is_empty() {
        let body = current.join("\n");
        sections.push((extract_path_hint(name, &body), current));
    }
    sections
}

#[derive(Debug, PartialEq, Eq)]
struct NumberedDiffLine {
    old: Option<usize>,
    new: Option<usize>,
    marker: char,
    content: String,
    header: bool,
}

fn parse_hunk_start(value: &str, marker: char) -> Option<usize> {
    value.strip_prefix(marker)?.split(',').next()?.parse().ok()
}

fn number_diff_lines(lines: &[String]) -> Vec<NumberedDiffLine> {
    let mut numbered = Vec::new();
    let mut old_line = None;
    let mut new_line = None;

    for line in lines {
        if line.starts_with("diff --git ") || line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        if line.starts_with("@@") {
            let mut fields = line.split_whitespace();
            let _ = fields.next();
            old_line = fields.next().and_then(|field| parse_hunk_start(field, '-'));
            new_line = fields.next().and_then(|field| parse_hunk_start(field, '+'));
            numbered.push(NumberedDiffLine {
                old: None,
                new: None,
                marker: ' ',
                content: line.clone(),
                header: true,
            });
            continue;
        }
        if line.starts_with("\\ No newline") {
            numbered.push(NumberedDiffLine {
                old: None,
                new: None,
                marker: ' ',
                content: line.clone(),
                header: true,
            });
            continue;
        }

        let (marker, content) = match line.chars().next() {
            Some(marker @ ('+' | '-' | ' ')) => (marker, line[marker.len_utf8()..].to_string()),
            _ => (' ', line.clone()),
        };
        let (old, new) = match marker {
            '-' => {
                let old = old_line;
                old_line = old_line.map(|line| line + 1);
                (old, None)
            }
            '+' => {
                let new = new_line;
                new_line = new_line.map(|line| line + 1);
                (None, new)
            }
            _ => {
                let old = old_line;
                let new = new_line;
                old_line = old_line.map(|line| line + 1);
                new_line = new_line.map(|line| line + 1);
                (old, new)
            }
        };
        numbered.push(NumberedDiffLine {
            old,
            new,
            marker,
            content,
            header: false,
        });
    }
    numbered
}

fn render_numbered_diff(path: &str, diff: &[String], width: usize) -> Vec<Line<'static>> {
    let numbered = number_diff_lines(diff);
    let number_width = numbered
        .iter()
        .flat_map(|line| [line.old, line.new])
        .flatten()
        .max()
        .map(|line| line.to_string().len())
        .unwrap_or(1);
    let code = numbered
        .iter()
        .filter(|line| !line.header)
        .map(|line| line.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let highlighted = lang_from_path(path)
        .map(|lang| highlight_to_lines(lang, &code, &forge_syntax::HighlightTheme::default()));
    let mut code_index = 0;
    let mut rendered = Vec::with_capacity(numbered.len());

    for line in numbered {
        if line.header {
            let gutter_width = number_width * 2 + 7;
            let text = format!(
                "{}{content}",
                " ".repeat(gutter_width),
                content = line.content
            );
            let padding = " ".repeat(width.saturating_sub(text.chars().count()));
            rendered.push(Line::from(Span::styled(
                format!("{text}{padding}"),
                theme::diff_hunk(),
            )));
            continue;
        }

        let old = line.old.map(|line| line.to_string()).unwrap_or_default();
        let new = line.new.map(|line| line.to_string()).unwrap_or_default();
        let line_style = match line.marker {
            '+' => theme::diff_add(),
            '-' => theme::diff_remove(),
            _ => theme::diff_context(),
        };
        let gutter = format!(
            "  {old:>number_width$} {new:>number_width$} │ {} ",
            line.marker
        );
        let row_width = gutter.chars().count() + line.content.chars().count();
        let mut spans = vec![Span::styled(gutter, line_style)];

        if let Some(Some(parts)) = highlighted.as_ref().map(|lines| lines.get(code_index)) {
            for (text, rgb, bold, italic) in parts {
                let mut style = ratatui::style::Style::default()
                    .fg(ratatui::style::Color::Rgb(rgb.0, rgb.1, rgb.2))
                    .bg(line_style.bg.unwrap_or(theme::PANEL_ALT));
                if *bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if *italic {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                spans.push(Span::styled(text.clone(), style));
            }
        } else {
            spans.push(Span::styled(line.content, line_style));
        }
        spans.push(Span::styled(
            " ".repeat(width.saturating_sub(row_width)),
            line_style,
        ));
        rendered.push(Line::from(spans));
        code_index += 1;
    }

    rendered
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

fn tool_argument<'a>(call: Option<&'a ToolCall>, name: &str) -> Option<&'a str> {
    call?.arguments.get(name)?.as_str().map(str::trim)
}

fn visible_result_count(detail: &str) -> usize {
    detail
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with("fff:")
        })
        .count()
}

fn result_count_label(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

fn group_routine_activity(items: Vec<ChatItem>) -> Vec<ChatItem> {
    let mut grouped = Vec::new();
    let mut pending: Vec<ChatItem> = Vec::new();

    for item in items {
        if let Some(category) = item_routine_category(&item) {
            if pending
                .first()
                .and_then(item_routine_category)
                .is_some_and(|current| current == category)
            {
                pending.push(item);
            } else {
                flush_activity_group(&mut grouped, &mut pending);
                pending.push(item);
            }
        } else {
            flush_activity_group(&mut grouped, &mut pending);
            grouped.push(item);
        }
    }
    flush_activity_group(&mut grouped, &mut pending);
    grouped
}

fn flush_activity_group(grouped: &mut Vec<ChatItem>, pending: &mut Vec<ChatItem>) {
    if pending.is_empty() {
        return;
    }
    if pending.len() == 1 {
        grouped.push(pending.pop().expect("pending item"));
        return;
    }
    let category = pending
        .first()
        .and_then(item_routine_category)
        .expect("routine group");
    let summary = activity_group_summary(category, pending);
    let detail = pending
        .iter()
        .map(activity_group_detail)
        .collect::<Vec<_>>()
        .join("\n---\n");
    grouped.push(ChatItem::ActivityGroup {
        category,
        summary,
        detail,
        state: ToolCardState::Done,
    });
    pending.clear();
}

fn item_routine_category(item: &ChatItem) -> Option<ActivityCategory> {
    match item {
        ChatItem::ToolCard {
            name,
            summary,
            state: ToolCardState::Done,
            ..
        } => routine_tool_category(name, summary, None),
        _ => None,
    }
}

fn routine_tool_category(
    name: &str,
    summary: &str,
    call: Option<&ToolCall>,
) -> Option<ActivityCategory> {
    match name {
        "read_file" | "fffind" | "ffgrep" | "fffind_files" | "ffgrep_files" => {
            Some(ActivityCategory::Exploring)
        }
        "git"
            if tool_argument(call, "subcommand").is_some_and(is_read_only_git)
                || summary
                    .split_whitespace()
                    .nth(1)
                    .is_some_and(is_read_only_git) =>
        {
            Some(ActivityCategory::Exploring)
        }
        "write_file" | "apply_patch" => Some(ActivityCategory::Implementing),
        "bash" if is_validation_command(summary.trim_start_matches("$ ")) => {
            Some(ActivityCategory::Validating)
        }
        _ => None,
    }
}

fn is_read_only_git(subcommand: &str) -> bool {
    matches!(
        subcommand,
        "status" | "diff" | "log" | "show" | "branch" | "rev-parse" | "ls-files" | "blame"
    )
}

fn is_validation_command(command: &str) -> bool {
    let command = command.trim();
    [
        "cargo test",
        "cargo check",
        "cargo fmt",
        "cargo clippy",
        "npm test",
        "pnpm test",
        "yarn test",
        "pytest",
        "go test",
    ]
    .iter()
    .any(|prefix| command.starts_with(prefix))
}

fn running_activity_summary(category: ActivityCategory, name: &str) -> String {
    match category {
        ActivityCategory::Exploring => format!("Reading via {name}"),
        ActivityCategory::Implementing => format!("Changing files via {name}"),
        ActivityCategory::Validating => name.to_string(),
        ActivityCategory::Reviewing => "Inspecting results".into(),
        ActivityCategory::Recovering => "Restoring task state".into(),
        ActivityCategory::Waiting => "Awaiting next event".into(),
    }
}

fn activity_group_summary(category: ActivityCategory, items: &[ChatItem]) -> String {
    match category {
        ActivityCategory::Exploring => {
            let reads = items
                .iter()
                .filter(
                    |item| matches!(item, ChatItem::ToolCard { name, .. } if name == "read_file"),
                )
                .count();
            let searches = items
                .iter()
                .filter(|item| matches!(item, ChatItem::ToolCard { name, .. } if name == "fffind" || name == "ffgrep"))
                .count();
            join_counts(&[
                (reads, "file inspected", "files inspected"),
                (searches, "search", "searches"),
            ])
        }
        ActivityCategory::Implementing => {
            let changed = items
                .iter()
                .filter(|item| match item {
                    ChatItem::DiffCard { .. } => true,
                    ChatItem::ToolCard { name, .. } => {
                        name == "write_file" || name == "apply_patch"
                    }
                    _ => false,
                })
                .count();
            result_count_label(changed, "file changed", "files changed")
        }
        ActivityCategory::Validating => items
            .iter()
            .filter_map(|item| match item {
                ChatItem::ToolCard { summary, .. } => summary.split(" · ").next(),
                _ => None,
            })
            .next()
            .unwrap_or("validation command")
            .to_string(),
        ActivityCategory::Reviewing => result_count_label(items.len(), "review", "reviews"),
        ActivityCategory::Recovering => "Restored previous task state".into(),
        ActivityCategory::Waiting => result_count_label(items.len(), "wait", "waits"),
    }
}

fn join_counts(counts: &[(usize, &str, &str)]) -> String {
    let parts = counts
        .iter()
        .filter(|(count, _, _)| *count > 0)
        .map(|(count, singular, plural)| result_count_label(*count, singular, plural))
        .collect::<Vec<_>>();
    if parts.is_empty() {
        "activity completed".into()
    } else {
        parts.join(" · ")
    }
}

fn activity_group_detail(item: &ChatItem) -> String {
    match item {
        ChatItem::ToolCard {
            name,
            summary,
            detail,
            ..
        } => format!("{name}: {summary}\n{detail}"),
        ChatItem::DiffCard { path, lines, .. } => format!("diff: {path}\n{}", lines.join("\n")),
        ChatItem::SessionRecovery {
            session_id,
            journal_path,
            last_seq,
            ..
        } => {
            format!("session recovery: {session_id}\n{journal_path}\nlast seq {last_seq}")
        }
        ChatItem::ContextHandoff { goal, .. } => format!("context handoff: {goal}"),
        _ => String::new(),
    }
}

fn classify_tool_content(
    name: &str,
    content: &str,
    call: Option<&ToolCall>,
) -> (ToolCardState, String, String) {
    let detail = redact_tool_output(content);
    let lower = detail.to_ascii_lowercase();
    let state = if lower.contains("validation") || lower.contains("denied by acl") {
        ToolCardState::Error
    } else if lower.contains("hitl") || lower.contains("awaiting") {
        ToolCardState::Blocked
    } else {
        ToolCardState::Done
    };
    let first = detail.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let count = visible_result_count(&detail);
    let summary = match name {
        "read_file" => {
            let label = result_count_label(count, "line", "lines");
            tool_argument(call, "path")
                .map(|path| format!("{path} · {label}"))
                .unwrap_or(label)
        }
        "bash" => {
            let label = if detail.trim().is_empty() {
                "completed".to_string()
            } else {
                result_count_label(count, "output line", "output lines")
            };
            tool_argument(call, "command")
                .map(redact_tool_output)
                .filter(|command| command != "[redacted tool output]")
                .map(|command| format!("$ {command} · {label}"))
                .unwrap_or(label)
        }
        "git" => {
            let command = tool_argument(call, "subcommand")
                .map(|subcommand| format!("git {subcommand}"))
                .unwrap_or_else(|| "git".to_string());
            if detail.trim().is_empty() {
                command
            } else {
                format!(
                    "{command} · {}",
                    result_count_label(count, "output line", "output lines")
                )
            }
        }
        "fffind" => {
            let label = if lower.contains("no matches found") {
                "no matches".to_string()
            } else {
                result_count_label(count, "file", "files")
            };
            tool_argument(call, "query")
                .map(|query| format!("{query} · {label}"))
                .unwrap_or(label)
        }
        "ffgrep" => {
            let label = if lower.contains("no matches found") {
                "no matches".to_string()
            } else {
                result_count_label(count, "match", "matches")
            };
            tool_argument(call, "pattern")
                .map(|pattern| format!("{pattern} · {label}"))
                .unwrap_or(label)
        }
        "web_search" => {
            let results = detail
                .lines()
                .filter(|line| {
                    let line = line.trim_start();
                    line.split_once(". **")
                        .is_some_and(|(index, _)| index.parse::<usize>().is_ok())
                })
                .count();
            let label = if lower.contains("no results") {
                "no results".to_string()
            } else {
                result_count_label(results, "result", "results")
            };
            tool_argument(call, "query")
                .map(|query| format!("{query} · {label}"))
                .unwrap_or(label)
        }
        _ if name.contains("write") || name.contains("search_replace") || name == "edit" => {
            format!("wrote · {}", first.chars().take(80).collect::<String>())
        }
        _ => detail.chars().take(160).collect(),
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

type HighlightSegment = (String, (u8, u8, u8), bool, bool);

fn render_highlighted_line(segments: &[HighlightSegment]) -> Vec<Span<'static>> {
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

#[cfg(test)]
pub struct ConversationWidget<'a> {
    pub model: &'a ConversationModel,
}

pub struct ConversationLinesWidget<'a> {
    pub lines: &'a [Line<'static>],
    pub tail_lines: &'a [Line<'static>],
    pub scroll: u16,
    pub follow: bool,
}

fn render_conversation_lines(
    lines: &[Line<'static>],
    tail_lines: &[Line<'static>],
    scroll_from_bottom: u16,
    follow: bool,
    area: Rect,
    buf: &mut Buffer,
) {
    let total = lines.len().saturating_add(tail_lines.len());
    let max_scroll = total.saturating_sub(area.height as usize);
    let scroll = if follow {
        max_scroll
    } else {
        max_scroll.saturating_sub((scroll_from_bottom as usize).min(max_scroll))
    };
    let end = scroll.saturating_add(area.height as usize).min(total);
    let visible = (scroll..end)
        .map(|index| {
            if index < lines.len() {
                lines[index].clone()
            } else {
                tail_lines[index - lines.len()].clone()
            }
        })
        .collect::<Vec<_>>();
    Paragraph::new(visible).render(area, buf);
}

impl Widget for ConversationLinesWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        render_conversation_lines(
            self.lines,
            self.tail_lines,
            self.scroll,
            self.follow,
            area,
            buf,
        );
    }
}

#[cfg(test)]
impl Widget for ConversationWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // The transcript owns the main area; hierarchy comes from spacing and
        // semantic markers rather than a permanent frame.
        let inset_x = 2.min(area.width);
        let inset_y = 1.min(area.height);
        let area = Rect {
            x: area.x.saturating_add(inset_x),
            y: area.y.saturating_add(inset_y),
            width: area.width.saturating_sub(inset_x),
            height: area.height.saturating_sub(inset_y),
        };
        let lines = self.model.lines_for_width(area.width as usize);
        render_conversation_lines(&lines, &[], self.model.scroll, self.model.follow, area, buf);
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
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

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
        // System prompts and reasoning stay hidden; tool results become compact cards.
        assert!(matches!(m.items[0], ChatItem::User { .. }));
        assert!(matches!(m.items[1], ChatItem::Assistant { .. }));
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
        let content = std::iter::repeat_n("word", 24)
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
    fn active_thinking_is_hidden_from_rendered_lines() {
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

        let rendered_text = rendered_text(&model);
        assert!(
            !rendered_text.contains("one two three"),
            "active reasoning must not appear in chat: {rendered_text}"
        );
        // The only visible content should be the empty assistant placeholder (if any).
        assert!(model.items.is_empty() || rendered_text.is_empty());
    }

    #[test]
    fn assistant_output_remains_visible_while_thinking_is_hidden() {
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
        assert_eq!(
            thought_lines,
            0,
            "active reasoning must not produce visible rows, got:\n{}",
            rendered_lines.join("\n")
        );
        assert!(
            rendered_lines.iter().any(|line| line.contains("ans")),
            "assistant output must remain visible, got:\n{}",
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
        assert!(rendered.starts_with("› hello world"), "{rendered}");
        let first = m.lines().into_iter().next().expect("operator turn");
        assert_eq!(first.style.bg, Some(theme::USER_BG));
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
    fn long_assistant_responses_use_no_repeated_product_heading() {
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
        assert!(!rendered.iter().any(|line| line.as_str() == "Forge"));
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
    fn builtin_tool_outputs_are_compact_until_expanded() {
        let calls = [
            (
                "read",
                "read_file",
                serde_json::json!({"path": "src/lib.rs"}),
            ),
            (
                "bash",
                "bash",
                serde_json::json!({"command": "cargo test --quiet"}),
            ),
            ("find", "fffind", serde_json::json!({"query": "*.rs"})),
            ("grep", "ffgrep", serde_json::json!({"pattern": "ToolCard"})),
            (
                "git",
                "git",
                serde_json::json!({"subcommand": "status", "args": ["--short"]}),
            ),
            (
                "web",
                "web_search",
                serde_json::json!({"query": "ratatui diff rendering"}),
            ),
        ]
        .map(|(id, name, arguments)| ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        });
        let mut messages = vec![Message {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: calls.to_vec(),
        }];
        let outputs = [
            ("read", "read_file", "pub fn noisy() {\n- old\n+ new\n}"),
            ("bash", "bash", "running tests\nfeature-a\n+ experimental"),
            ("find", "fffind", "src/lib.rs\nsrc/main.rs"),
            (
                "grep",
                "ffgrep",
                "src/lib.rs:10:ToolCard\nsrc/app.rs:20:ToolCard",
            ),
            ("git", "git", " M src/lib.rs\n M src/app.rs"),
            (
                "web",
                "web_search",
                "## Web search: ratatui diff rendering\n\n1. **Ratatui**\n   - URL: https://ratatui.rs\n   - Snippet: Widgets\n\n```json\n[]\n```",
            ),
        ];
        let output_count = outputs.len();
        messages.extend(outputs.map(|(id, name, content)| Message {
            role: MessageRole::Tool,
            content: content.into(),
            tool_call_id: Some(id.into()),
            name: Some(name.into()),
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        }));

        let model = ConversationModel::from_messages(
            &messages,
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        );
        assert_eq!(output_count, outputs.len());
        assert!(model.items.iter().any(|item| matches!(
            item,
            ChatItem::ActivityGroup {
                category: ActivityCategory::Exploring,
                ..
            }
        )));
        assert!(
            !model
                .items
                .iter()
                .any(|item| matches!(item, ChatItem::DiffCard { .. })),
            "ordinary multiline output must not be classified as a diff"
        );

        let rendered = model
            .lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        for summary in [
            "Explored repository",
            "2 searches",
            "$ cargo test --quiet · 3 output lines",
            "ratatui diff rendering · 1 result",
        ] {
            assert!(rendered.contains(summary), "missing {summary}:\n{rendered}");
        }
        for hidden in [
            "pub fn noisy",
            "feature-a",
            "https://ratatui.rs",
            "ToolCard · 2 matches",
        ] {
            assert!(
                !rendered.contains(hidden),
                "tool detail leaked into the compact card: {hidden}\n{rendered}"
            );
        }

        let expanded = ConversationModel::from_messages(
            &messages,
            &[],
            SessionStatus::Running,
            ConversationViewOpts {
                tool_expanded: true,
                ..ConversationViewOpts::default()
            },
        )
        .lines()
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
        assert!(expanded.contains("https://ratatui.rs"), "{expanded}");
    }

    #[test]
    fn routine_activity_groups_respect_boundaries_and_failures() {
        let model = ConversationModel {
            items: group_routine_activity(vec![
                ChatItem::ToolCard {
                    name: "read_file".into(),
                    summary: "a.rs · 2 lines".into(),
                    detail: "a".into(),
                    state: ToolCardState::Done,
                    duration: None,
                },
                ChatItem::ToolCard {
                    name: "ffgrep".into(),
                    summary: "needle · 1 match".into(),
                    detail: "a.rs:1:needle".into(),
                    state: ToolCardState::Done,
                    duration: None,
                },
                ChatItem::User {
                    text: "stop".into(),
                },
                ChatItem::ToolCard {
                    name: "bash".into(),
                    summary: "$ cargo test · failed".into(),
                    detail: "status 101".into(),
                    state: ToolCardState::Error,
                    duration: None,
                },
            ]),
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };

        let text = rendered_text(&model);
        assert!(text.contains("✓ Explored repository"));
        assert!(text.contains("› stop"));
        assert!(text.contains("✗ bash"));
        assert!(!text.contains("Validation completed"));
    }

    #[test]
    fn grouped_activity_renders_running_completed_and_details() {
        let running = ConversationModel::from_messages(
            &[],
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        )
        .with_running_tool("read_file");
        let running_text = rendered_text(&running);
        assert!(running_text.contains("● Exploring repository"));
        assert!(running_text.contains("Reading via read_file"));

        let completed = ConversationModel {
            items: group_routine_activity(vec![
                ChatItem::ToolCard {
                    name: "write_file".into(),
                    summary: "wrote · src/lib.rs".into(),
                    detail: "src/lib.rs".into(),
                    state: ToolCardState::Done,
                    duration: None,
                },
                ChatItem::ToolCard {
                    name: "apply_patch".into(),
                    summary: "wrote · src/app.rs".into(),
                    detail: "src/app.rs".into(),
                    state: ToolCardState::Done,
                    duration: None,
                },
            ]),
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts {
                tool_expanded: true,
                ..Default::default()
            },
        };
        let text = rendered_text(&completed);
        assert!(text.contains("✓ Implemented changes"));
        assert!(text.contains("2 files changed"));
        assert!(text.contains("Details are also available in Activity."));
        assert!(text.contains("write_file: wrote · src/lib.rs"));
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
            "Correcting an invalid tool request",
            "No command was executed and no files were changed.",
            "Retrying with corrected tool details 2/3",
            "Retrying",
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
            rendered.contains("src/lib.rs  diff"),
            "file path should be shown in the diff card: {rendered}"
        );
        assert!(rendered.contains("1   │ - old"), "{rendered}");
        assert!(rendered.contains("  1 │ + new"), "{rendered}");
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
    fn multi_file_diff_results_become_separate_cards() {
        let msgs = vec![Message {
            role: MessageRole::Tool,
            content: concat!(
                "diff --git a/src/a.rs b/src/a.rs\n",
                "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -2 +2 @@\n-old_a\n+new_a\n",
                "\n",
                "diff --git a/src/b.rs b/src/b.rs\n",
                "--- a/src/b.rs\n+++ b/src/b.rs\n@@ -7 +7 @@\n-old_b\n+new_b\n"
            )
            .into(),
            tool_call_id: Some("1".into()),
            name: Some("apply_patch".into()),
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

        assert_eq!(model.items.len(), 2);
        assert!(matches!(
            &model.items[0],
            ChatItem::DiffCard { path, lines, .. }
                if path == "src/a.rs" && lines.last().is_some_and(|line| line == "+new_a")
        ));
        assert!(matches!(
            &model.items[1],
            ChatItem::DiffCard { path, .. } if path == "src/b.rs"
        ));
    }

    #[test]
    fn diff_line_numbers_track_additions_removals_and_context() {
        let diff = [
            "@@ -10,3 +20,4 @@",
            " context",
            "-removed",
            "+added",
            "+extra",
            " tail",
        ]
        .map(str::to_string);

        let numbered = number_diff_lines(&diff);

        assert_eq!(
            numbered
                .iter()
                .map(|line| (line.old, line.new, line.marker))
                .collect::<Vec<_>>(),
            vec![
                (None, None, ' '),
                (Some(10), Some(20), ' '),
                (Some(11), None, '-'),
                (None, Some(21), '+'),
                (None, Some(22), '+'),
                (Some(12), Some(23), ' '),
            ]
        );
    }

    #[test]
    fn diff_rows_have_backgrounds_and_syntax_highlighting() {
        let diff = ["@@ -1 +1 @@", "-fn old() {}", "+fn new() {}"].map(str::to_string);

        let rendered = render_numbered_diff("src/lib.rs", &diff, 40);
        let removed = &rendered[1];
        let added = &rendered[2];

        assert!(removed
            .spans
            .iter()
            .all(|span| span.style.bg == Some(theme::DIFF_REMOVE_BG)));
        assert!(added
            .spans
            .iter()
            .all(|span| span.style.bg == Some(theme::DIFF_ADD_BG)));
        assert!(
            added.spans.len() > 3,
            "Rust tokens should be separate spans"
        );
        assert_eq!(
            added.spans.iter().map(|span| span.width()).sum::<usize>(),
            40
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
    fn conversation_widget_applies_padding_offsets() {
        let m = ConversationModel::from_messages(
            &[],
            &[],
            SessionStatus::Running,
            ConversationViewOpts::default(),
        )
        .with_brand("forge 0.8.0")
        .with_home("workspace".into(), 2);
        let area = Rect::new(0, 0, 40, 8);
        let backend = TestBackend::new(area.width, area.height);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|frame| {
            frame.render_widget(ConversationWidget { model: &m }, area);
        })
        .unwrap();
        let buf = term.backend().buffer();
        assert_eq!(buf[(0, 0)].symbol(), " ");
        assert_eq!(buf[(1, 0)].symbol(), " ");
        assert_ne!(buf[(0, 1)].symbol(), "F");
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
        assert!(text.contains("Responding"));
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
        assert!(text.contains("● Exploring repository"));
        assert!(text.contains("Reading via read_file"));
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
        assert!(text.contains("Continuing in a fresh context"));
        assert!(text.contains("rate limiting middleware"));
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
        assert!(text.contains("Continuing from the restored journal."));
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
            "Review",
            "SENSOR · DETERMINISTIC",
            "Findings, evidence, severity, and suggested actions.",
            "Finding:",
            "suggested action sent back to the task",
            "Repairing",
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

    fn rendered_text(model: &ConversationModel) -> String {
        model
            .lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }
}
