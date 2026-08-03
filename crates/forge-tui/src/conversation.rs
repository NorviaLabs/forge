//! Conversation view model (TUI-02) — polished chat, thinking, tools, diffs.

use crate::theme;
use crate::user_message_gutter;
use forge_core::{AgentSession, TurnEvent, TURN_FAILED_MARKER};
use forge_syntax::highlight_to_lines;
use forge_types::{Message, MessageRole, TaskLifecycle, ToolCall};
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
    TaskLifecycle {
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

#[derive(Debug, Clone, PartialEq)]
pub enum ConversationBlock {
    UserMessage(UserMessagePresentation),
    AssistantAnswer(AssistantAnswerPresentation),
    ActiveProgress(ActiveProgressPresentation),
    ActivityGroup(ActivityGroupPresentation),
    Callout(CalloutPresentation),
    CodeBlock(CodeBlockPresentation),
    DiffBlock(DiffBlockPresentation),
    Metadata(MetadataPresentation),
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserMessagePresentation {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssistantAnswerPresentation {
    pub text: String,
    pub streaming: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveProgressPresentation {
    pub id: String,
    pub label: String,
    pub summary: String,
    pub status: ActiveProgressStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveProgressStatus {
    Started,
    Updated,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivityGroupPresentation {
    pub id: String,
    pub label: String,
    pub count_label: String,
    pub outcome: ActivityOutcome,
    pub expanded: bool,
    pub items: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityOutcome {
    Success,
    Neutral,
    Warning,
    Failure,
    Blocked,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CalloutPresentation {
    pub text: String,
    pub kind: BannerKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodeBlockPresentation {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiffBlockPresentation {
    pub path: String,
    pub lines: Vec<String>,
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetadataPresentation {
    pub text: String,
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
    pub fn semantic_blocks(&self) -> Vec<ConversationBlock> {
        semantic_blocks_from_items(&self.items, self.opts.tool_expanded)
    }

    pub fn from_messages(
        messages: &[Message],
        events: &[TurnEvent],
        status: TaskLifecycle,
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
                    // Terminal failure summaries are durable assistant messages with a
                    // structural marker — never treated as final answers.
                    if let Some(summary) = m.content.strip_prefix(TURN_FAILED_MARKER) {
                        if m.tool_calls.is_empty() && !summary.trim().is_empty() {
                            items.push(ChatItem::Banner {
                                text: summary.trim().to_string(),
                                kind: BannerKind::Error,
                            });
                        }
                        continue;
                    }
                    // Final answer is durable content only. Thinking/reasoning never
                    // becomes AssistantAnswer (provenance: primary text channel).
                    let effective_text = sanitize_final_answer_text(&m.content);
                    if !effective_text.trim().is_empty() {
                        if repair_pending {
                            items.push(ChatItem::GeneratorRepair {
                                text: effective_text.clone(),
                            });
                            repair_pending = false;
                        } else if validation_retry_pending {
                            items.push(ChatItem::RetryAssistant {
                                text: effective_text.clone(),
                            });
                            validation_retry_pending = false;
                        } else if m.tool_calls.is_empty() {
                            items.push(ChatItem::Assistant {
                                text: effective_text,
                            });
                        }
                    }
                }
                // Tool results are not shown as chat messages (keeps the transcript clean).
                MessageRole::Tool => {
                    let name = m.name.as_deref().unwrap_or("tool");
                    if m.content.starts_with("Tool validation error:")
                        || m.content.contains("validation retry budget exceeded")
                    {
                        validation_retry_pending = true;
                        let retry = validation_failures.entry(name.to_string()).or_default();
                        *retry += 1;
                        // Keep raw validator detail in activity evidence, not main transcript.
                        let error = m
                            .content
                            .trim_start_matches("Tool validation error: ")
                            .to_string();
                        items.push(ChatItem::ToolCard {
                            name: name.to_string(),
                            summary: format!("invalid arguments · retry {retry}"),
                            detail: error,
                            state: ToolCardState::Error,
                            duration: None,
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
                // `MessageRole` is `#[non_exhaustive]`. A role this build cannot present is
                // skipped rather than rendered under another role: misattributing authorship
                // in the transcript is worse than omitting one message.
                _ => {}
            }
        }
        let mut latest_progress = None;
        for event in events {
            if event.kind == "progress" || event.kind == "thinking" {
                latest_progress = Some(event.detail.clone());
            }
        }
        if let Some(text) = latest_progress {
            if status == TaskLifecycle::Working {
                items.push(ChatItem::Thinking {
                    text,
                    duration_secs: None,
                });
            }
        }
        items = group_routine_activity(items);
        if status == TaskLifecycle::Waiting {
            items.push(ChatItem::Banner {
                text: "Awaiting approval · Enter/a allow once · s remember exact when eligible · d/Esc deny"
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
        Self::from_messages(
            &session.messages,
            &session.events,
            session.active_task.lifecycle,
            opts,
        )
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
        let mut lines = Vec::new();
        let gap = !self.opts.compact;
        for block in self.semantic_blocks() {
            match block {
                ConversationBlock::UserMessage(p) => {
                    let mut user_lines = user_message_gutter::render_user_message_lines(
                        &p.text,
                        width,
                        &crate::theme::active(),
                        false,
                        wrap,
                    );
                    for line in &mut user_lines {
                        let padding = width.saturating_sub(line.width());
                        if padding > 0 {
                            line.spans.insert(0, Span::raw(" ".repeat(padding)));
                        }
                    }
                    lines.extend(user_lines);
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::AssistantAnswer(p) => {
                    let parts = assistant_lines(&p.text, width);
                    for (i, line) in parts.into_iter().enumerate() {
                        if i == 0 {
                            let styled = line.clone().style(theme::assistant_answer_style());
                            let mut with_marker = vec![Span::styled("▍ ", theme::agent())];
                            with_marker.extend(styled.spans);
                            lines.push(Line::from(with_marker));
                        } else {
                            lines.push(line.style(theme::assistant_answer_style()));
                        }
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::ActiveProgress(p) => {
                    let label = format!("{} · {}", p.label, p.summary);
                    let prefix = if p.id == "stream" { "▍ " } else { "● " };
                    lines.push(Line::from(vec![
                        Span::styled(prefix, theme::progress_style()),
                        Span::styled(label, theme::text().add_modifier(Modifier::BOLD)),
                        Span::styled("  ", theme::metadata_style()),
                        Span::styled(
                            match p.status {
                                ActiveProgressStatus::Started => "started",
                                ActiveProgressStatus::Updated => "updated",
                                ActiveProgressStatus::Completed => "completed",
                                ActiveProgressStatus::Failed => "failed",
                            },
                            theme::metadata_style(),
                        ),
                    ]));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::ActivityGroup(p) => {
                    let st = match p.outcome {
                        ActivityOutcome::Success => theme::ok(),
                        ActivityOutcome::Neutral => theme::muted(),
                        ActivityOutcome::Warning => theme::warn(),
                        ActivityOutcome::Failure => theme::danger(),
                        ActivityOutcome::Blocked => theme::warn(),
                    };
                    let prefix = match p.outcome {
                        ActivityOutcome::Success => "✓ ",
                        ActivityOutcome::Failure => "✗ ",
                        ActivityOutcome::Blocked => "⏸ ",
                        ActivityOutcome::Warning => "!",
                        ActivityOutcome::Neutral => "● ",
                    };
                    lines.push(Line::from(vec![
                        Span::styled(prefix, st),
                        Span::styled(p.label, theme::text().add_modifier(Modifier::BOLD)),
                        Span::styled("  ", theme::metadata_style()),
                        Span::styled(p.count_label, theme::metadata_style()),
                        Span::styled(activity_detail_label(p.expanded), theme::metadata_style()),
                    ]));
                    if p.expanded {
                        for item in p.items {
                            for line in wrap(&item, width.saturating_sub(2)) {
                                lines.push(Line::from(Span::styled(
                                    format!("  {line}"),
                                    theme::muted(),
                                )));
                            }
                        }
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::Callout(p) => {
                    let st = match p.kind {
                        BannerKind::Info => theme::info(),
                        BannerKind::Warn => theme::warn(),
                        BannerKind::Error => theme::error_callout(),
                        BannerKind::Ok => theme::ok(),
                    };
                    for l in wrap(&p.text, width) {
                        lines.push(Line::from(Span::styled(format!("▸ {l}"), st)));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::CodeBlock(p) => {
                    for line in assistant_lines(&p.text, width) {
                        lines.push(line.style(theme::code_block()));
                    }
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::DiffBlock(p) => {
                    let (tag, st) = ("✓", theme::tool_success_style());
                    lines.push(Line::from(vec![
                        Span::styled(format!("{tag} "), st),
                        Span::styled(p.path.clone(), theme::text().add_modifier(Modifier::BOLD)),
                        Span::styled("  diff", theme::dim()),
                    ]));
                    if !p.rationale.is_empty() {
                        for l in wrap(&p.rationale, width.saturating_sub(4))
                            .into_iter()
                            .take(2)
                        {
                            lines.push(Line::from(vec![
                                Span::styled("  ", theme::info()),
                                Span::styled(l, theme::muted().add_modifier(Modifier::ITALIC)),
                            ]));
                        }
                    }
                    lines.extend(render_numbered_diff(&p.path, &p.lines, width));
                    if gap {
                        lines.push(Line::from(""));
                    }
                }
                ConversationBlock::Metadata(p) => {
                    for l in wrap(&p.text, width) {
                        lines.push(Line::from(Span::styled(l, theme::muted())));
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

fn semantic_blocks_from_items(items: &[ChatItem], tool_expanded: bool) -> Vec<ConversationBlock> {
    let mut blocks = Vec::new();
    let mut progress: Option<ActiveProgressPresentation> = None;
    let mut activity_group: Option<ActivityGroupPresentation> = None;

    let flush_progress =
        |blocks: &mut Vec<ConversationBlock>, progress: &mut Option<ActiveProgressPresentation>| {
            if let Some(p) = progress.take() {
                blocks.push(ConversationBlock::ActiveProgress(p));
            }
        };

    let flush_activity = |blocks: &mut Vec<ConversationBlock>,
                          group: &mut Option<ActivityGroupPresentation>| {
        if let Some(mut item) = group.take() {
            item.expanded = tool_expanded && matches!(item.outcome, ActivityOutcome::Failure);
            blocks.push(ConversationBlock::ActivityGroup(item));
        }
    };

    for item in items {
        match item {
            ChatItem::User { text } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::UserMessage(UserMessagePresentation {
                    text: text.clone(),
                }));
            }
            ChatItem::Thinking { text, .. } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::ActiveProgress(
                    ActiveProgressPresentation {
                        id: "thinking".into(),
                        label: "Thinking".into(),
                        summary: text.clone(),
                        status: ActiveProgressStatus::Updated,
                    },
                ));
            }
            ChatItem::Assistant { text } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::AssistantAnswer(
                    AssistantAnswerPresentation {
                        text: text.clone(),
                        streaming: false,
                    },
                ));
            }
            ChatItem::ActivitySummary {
                label,
                action,
                kind,
            } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                let mut text = label.clone();
                if let Some(action) = action {
                    text.push_str(" · ");
                    text.push_str(action);
                }
                blocks.push(ConversationBlock::Callout(CalloutPresentation {
                    text,
                    kind: *kind,
                }));
            }
            ChatItem::StreamingAssistant { text } => {
                // Streaming final-answer deltas are answer provenance, not progress.
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::AssistantAnswer(
                    AssistantAnswerPresentation {
                        text: sanitize_final_answer_text(text),
                        streaming: true,
                    },
                ));
            }
            ChatItem::RetryAssistant { text }
            | ChatItem::GeneratorRepair { text }
            | ChatItem::EvaluatorReport { text } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::Callout(CalloutPresentation {
                    text: text.clone(),
                    kind: BannerKind::Warn,
                }));
            }
            ChatItem::ValidationFailure { tool, error, retry } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::Callout(CalloutPresentation {
                    text: format!("{tool}: {error} (retry {retry})"),
                    kind: BannerKind::Error,
                }));
            }
            ChatItem::ToolCard {
                name,
                summary,
                detail,
                state,
                ..
            } => {
                if let Some(entry) = activity_entry_from_tool(name, summary, detail, *state) {
                    append_activity_entry(&mut blocks, &mut activity_group, entry);
                } else {
                    flush_progress(&mut blocks, &mut progress);
                    flush_activity(&mut blocks, &mut activity_group);
                    blocks.push(ConversationBlock::ActivityGroup(
                        ActivityGroupPresentation {
                            id: format!("tool:{name}:{summary}"),
                            label: name.clone(),
                            count_label: "1 item".into(),
                            outcome: match state {
                                ToolCardState::Running => ActivityOutcome::Neutral,
                                ToolCardState::Done => ActivityOutcome::Success,
                                ToolCardState::Blocked => ActivityOutcome::Blocked,
                                ToolCardState::Error => ActivityOutcome::Failure,
                            },
                            expanded: matches!(state, ToolCardState::Error) && tool_expanded,
                            items: vec![format!("{name}: {summary}\n{detail}")],
                        },
                    ));
                }
            }
            ChatItem::ActivityGroup {
                category,
                summary,
                detail,
                state,
            } => {
                flush_progress(&mut blocks, &mut progress);
                let outcome = match state {
                    ToolCardState::Running => ActivityOutcome::Neutral,
                    // Routine exploration is evidence, not a green "success" banner.
                    ToolCardState::Done if matches!(category, ActivityCategory::Exploring) => {
                        ActivityOutcome::Neutral
                    }
                    ToolCardState::Done => ActivityOutcome::Success,
                    ToolCardState::Blocked => ActivityOutcome::Blocked,
                    ToolCardState::Error => ActivityOutcome::Failure,
                };
                append_activity_entry(
                    &mut blocks,
                    &mut activity_group,
                    ActivityGroupPresentation {
                        id: format!("activity:{category:?}"),
                        label: category
                            .label(matches!(state, ToolCardState::Running))
                            .to_string(),
                        count_label: summary.clone(),
                        outcome,
                        expanded: matches!(state, ToolCardState::Error) && tool_expanded,
                        items: vec![detail.clone()],
                    },
                );
            }
            ChatItem::DiffCard {
                path,
                lines,
                rationale,
            } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::DiffBlock(DiffBlockPresentation {
                    path: path.clone(),
                    lines: lines.clone(),
                    rationale: rationale.clone(),
                }));
            }
            ChatItem::System { text } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::Metadata(MetadataPresentation {
                    text: text.clone(),
                }));
            }
            ChatItem::Brand { version } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::Metadata(MetadataPresentation {
                    text: format!("Forge {version}"),
                }));
            }
            ChatItem::Home {
                workspace,
                skills_loaded,
            } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::Metadata(MetadataPresentation {
                    text: format!("{workspace} · {skills_loaded} skills"),
                }));
            }
            ChatItem::ContextHandoff { goal, .. } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::Callout(CalloutPresentation {
                    text: goal.clone(),
                    kind: BannerKind::Info,
                }));
            }
            ChatItem::SessionRecovery { session_id, .. } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::Callout(CalloutPresentation {
                    text: format!("Restoring session {session_id}"),
                    kind: BannerKind::Info,
                }));
            }
            ChatItem::TaskLifecycle { status, model, .. } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::Metadata(MetadataPresentation {
                    text: format!("{status} · {model}"),
                }));
            }
            ChatItem::Queued { text, .. } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::ActiveProgress(
                    ActiveProgressPresentation {
                        id: "queued".into(),
                        label: "Queued".into(),
                        summary: text.clone(),
                        status: ActiveProgressStatus::Started,
                    },
                ));
            }
            ChatItem::Banner { text, kind } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::Callout(CalloutPresentation {
                    text: text.clone(),
                    kind: *kind,
                }));
            }
        }
    }
    flush_progress(&mut blocks, &mut progress);
    flush_activity(&mut blocks, &mut activity_group);
    compose_turn_presentation(blocks)
}

/// Completed-turn composition: within each user turn, emit
/// UserMessage → AssistantAnswer|TurnFailure → ActivityGroup.
/// ActiveProgress is kept only while no terminal answer/failure exists yet.
fn compose_turn_presentation(blocks: Vec<ConversationBlock>) -> Vec<ConversationBlock> {
    let mut out = Vec::with_capacity(blocks.len());
    let mut segment: Vec<ConversationBlock> = Vec::new();

    let flush_segment = |out: &mut Vec<ConversationBlock>, segment: &mut Vec<ConversationBlock>| {
        if segment.is_empty() {
            return;
        }
        let mut user = Vec::new();
        let mut answers = Vec::new();
        let mut failures = Vec::new();
        let mut activity = Vec::new();
        let mut progress = Vec::new();
        let mut other = Vec::new();
        for block in segment.drain(..) {
            match block {
                ConversationBlock::UserMessage(_) => user.push(block),
                ConversationBlock::AssistantAnswer(_) => answers.push(block),
                ConversationBlock::Callout(ref c) if matches!(c.kind, BannerKind::Error) => {
                    failures.push(block)
                }
                ConversationBlock::ActivityGroup(_) | ConversationBlock::DiffBlock(_) => {
                    activity.push(block)
                }
                ConversationBlock::ActiveProgress(_) => progress.push(block),
                other_block => other.push(other_block),
            }
        }
        // One durable final answer per turn: last primary answer wins.
        if answers.len() > 1 {
            let last = answers.pop().into_iter().collect::<Vec<_>>();
            answers = last;
        }
        // One terminal failure summary per turn.
        if failures.len() > 1 {
            let last = failures.pop().into_iter().collect::<Vec<_>>();
            failures = last;
        }
        out.extend(user);
        let terminal = !answers.is_empty() || !failures.is_empty();
        if !terminal {
            // Still streaming / waiting: progress may remain visible.
            out.extend(progress);
        }
        // Success: answer before activity. Failure: summary before activity.
        out.extend(answers);
        out.extend(failures);
        out.extend(activity);
        out.extend(other);
    };

    for block in blocks {
        match block {
            ConversationBlock::UserMessage(_) => {
                flush_segment(&mut out, &mut segment);
                segment.push(block);
            }
            other => segment.push(other),
        }
    }
    flush_segment(&mut out, &mut segment);
    out
}

/// Strip internal protocol control markers from final-answer text before render.
/// Not phrase filtering: only known structural envelopes (e.g. confidence tags).
fn sanitize_final_answer_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("\\confidence{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "\\confidence{".len()..];
        if let Some(end) = after.find('}') {
            rest = &after[end + 1..];
        } else {
            // Unclosed marker: drop remainder of control token only.
            break;
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

fn append_activity_entry(
    blocks: &mut Vec<ConversationBlock>,
    pending: &mut Option<ActivityGroupPresentation>,
    next: ActivityGroupPresentation,
) {
    if let Some(group) = pending.as_mut() {
        if group.id == next.id {
            group.count_label =
                result_count_label(group.items.len() + next.items.len(), "item", "items");
            group.outcome = merge_activity_outcomes(group.outcome, next.outcome);
            group.expanded |= next.expanded;
            group.items.extend(next.items);
            return;
        }
        if let Some(group) = pending.take() {
            blocks.push(ConversationBlock::ActivityGroup(group));
        }
    }
    *pending = Some(next);
}

fn merge_activity_outcomes(left: ActivityOutcome, right: ActivityOutcome) -> ActivityOutcome {
    use ActivityOutcome::*;
    match (left, right) {
        (Failure, _) | (_, Failure) => Failure,
        (Blocked, _) | (_, Blocked) => Blocked,
        (Warning, _) | (_, Warning) => Warning,
        (Success, _) | (_, Success) => Success,
        _ => Neutral,
    }
}

fn activity_entry_from_tool(
    name: &str,
    summary: &str,
    detail: &str,
    state: ToolCardState,
) -> Option<ActivityGroupPresentation> {
    let category = routine_tool_category(name, summary, None)?;
    let lower = detail.to_ascii_lowercase();
    let zero_result = lower.contains("no matches found") || lower.contains("no results");
    let outcome = match state {
        ToolCardState::Running => ActivityOutcome::Neutral,
        // Routine exploration success (including zero-result search) stays neutral.
        ToolCardState::Done if matches!(category, ActivityCategory::Exploring) || zero_result => {
            ActivityOutcome::Neutral
        }
        ToolCardState::Done => ActivityOutcome::Success,
        ToolCardState::Blocked => ActivityOutcome::Blocked,
        ToolCardState::Error => ActivityOutcome::Failure,
    };
    let label = match state {
        ToolCardState::Running => category.label(true).to_string(),
        _ => category.label(false).to_string(),
    };
    Some(ActivityGroupPresentation {
        id: format!("activity:{category:?}"),
        label,
        count_label: if matches!(state, ToolCardState::Running) {
            running_activity_summary(category, name)
        } else {
            result_count_label(1, "item", "items")
        },
        outcome,
        expanded: matches!(state, ToolCardState::Error),
        items: vec![format!("{name}: {summary}\n{detail}")],
    })
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

fn activity_detail_label(expanded: bool) -> &'static str {
    if expanded {
        "  · Ctrl+O collapse"
    } else {
        "  · Ctrl+O details"
    }
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
    let highlighted =
        lang_from_path(path).map(|lang| highlight_to_lines(lang, &code, &theme::syntax_theme()));
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
                    .bg(line_style.bg.unwrap_or(theme::panel_alt_bg()));
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
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(detail) {
        if let Some(hits) = value.get("hits").and_then(|hits| hits.as_array()) {
            return hits.len();
        }
    }
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

pub(crate) fn wrap(s: &str, width: usize) -> Vec<String> {
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
                push_code_block(&mut out, &language, &mut code_block_lines);
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
            let mut processed = raw.to_string();
            if let Some(rest) = processed.strip_prefix("# ") {
                processed = format!("**{}**", rest);
            } else if let Some(rest) = processed.strip_prefix("## ") {
                processed = format!("**{}**", rest);
            } else if let Some(rest) = processed.strip_prefix("- ") {
                processed = format!("• {}", rest);
            } else if let Some(rest) = processed.strip_prefix("* ") {
                processed = format!("• {}", rest);
            }
            let wrapped = wrap(&processed, width);
            for line in wrapped {
                out.push(Line::from(render_md_line(&line)));
            }
        }
    }

    // A streaming answer can end mid-block, before its closing fence arrives.
    // Render what has been received rather than discarding it: the opening fence
    // is already on screen, so dropping the body renders an empty code block and
    // the answer looks like the model produced nothing.
    //
    // No synthetic closing fence is emitted — the block genuinely is not closed,
    // and inventing a terminator would misrepresent the message.
    push_code_block(&mut out, &language, &mut code_block_lines);

    if out.is_empty() {
        out.push(Line::from(String::new()));
    }
    out
}

/// Render an accumulated fenced code block into `out` and clear the accumulator.
///
/// Shared by the closing-fence path and the end-of-text path so a partial block
/// renders identically to a complete one.
fn push_code_block(
    out: &mut Vec<Line<'static>>,
    language: &str,
    code_block_lines: &mut Vec<String>,
) {
    if code_block_lines.is_empty() {
        return;
    }
    let code = code_block_lines.join("\n");
    let theme = theme::syntax_theme();
    // Borrowed, not consumed: the highlight is shared with the cache, and
    // `render_highlighted_line` only needs a slice.
    for line_segments in highlight_to_lines(language, &code, &theme).iter() {
        out.push(Line::from(render_highlighted_line(line_segments)).style(theme::code_block()));
    }
    code_block_lines.clear();
}

fn render_md_line(line: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        if let Some(start) = rest.find("**") {
            if start > 0 {
                spans.push(Span::styled(rest[..start].to_string(), theme::text()));
            }
            let after = &rest[start + 2..];
            if let Some(end) = after.find("**") {
                spans.push(Span::styled(
                    after[..end].to_string(),
                    theme::text().add_modifier(Modifier::BOLD),
                ));
                rest = &after[end + 2..];
                continue;
            }
            spans.push(Span::styled("**".to_string() + after, theme::text()));
            break;
        } else if let Some(start) = rest.find('`') {
            if start > 0 {
                spans.push(Span::styled(rest[..start].to_string(), theme::text()));
            }
            let after = &rest[start + 1..];
            if let Some(end) = after.find('`') {
                spans.push(Span::styled(
                    after[..end].to_string(),
                    theme::tool().add_modifier(Modifier::BOLD),
                ));
                rest = &after[end + 1..];
                continue;
            }
            spans.push(Span::styled("`".to_string() + after, theme::text()));
            break;
        } else {
            spans.push(Span::styled(rest.to_string(), theme::text()));
            break;
        }
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), theme::text()));
    }
    spans
}

type HighlightSegment = (String, (u8, u8, u8), bool, bool);

fn render_highlighted_line(segments: &[HighlightSegment]) -> Vec<Span<'static>> {
    let block = theme::code_block();
    segments
        .iter()
        .map(|(text, rgb, bold, italic)| {
            let mut style = ratatui::style::Style::default()
                .fg(ratatui::style::Color::Rgb(rgb.0, rgb.1, rgb.2))
                .bg(block.bg.unwrap_or(theme::panel_alt_bg()));
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
    theme::fill(area, buf, theme::assistant_message());
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
            TaskLifecycle::Working,
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
        let semantic = m.semantic_blocks();
        assert!(
            semantic
                .iter()
                .any(|block| matches!(block, ConversationBlock::ActivityGroup(_))),
            "tool result should classify into semantic activity blocks: {semantic:?}"
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
    fn semantic_blocks_group_tool_activity_by_category() {
        let model = ConversationModel {
            items: vec![
                ChatItem::ToolCard {
                    name: "read_file".into(),
                    summary: "src/lib.rs · 2 lines".into(),
                    detail: "src/lib.rs".into(),
                    state: ToolCardState::Done,
                    duration: None,
                },
                ChatItem::ToolCard {
                    name: "fffind".into(),
                    summary: "needle · 1 file".into(),
                    detail: "src/main.rs".into(),
                    state: ToolCardState::Done,
                    duration: None,
                },
            ],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };

        let blocks = model.semantic_blocks();
        assert_eq!(blocks.len(), 1);
        assert!(matches!(
            &blocks[0],
            ConversationBlock::ActivityGroup(group)
                if group.label == "Explored repository"
                    && group.items.len() == 2
                    && group.count_label.contains("2")
        ));
    }

    #[test]
    fn semantic_blocks_replace_streaming_answer_updates() {
        let model = ConversationModel {
            items: vec![
                ChatItem::StreamingAssistant {
                    text: "working on the change".into(),
                },
                ChatItem::StreamingAssistant {
                    text: "working on the final change".into(),
                },
            ],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };

        let blocks = model.semantic_blocks();
        // Compose keeps one final answer (last streaming snapshot).
        assert_eq!(blocks.len(), 1);
        assert!(matches!(
            &blocks[0],
            ConversationBlock::AssistantAnswer(answer)
                if answer.text == "working on the final change" && answer.streaming
        ));
    }

    #[test]
    fn semantic_blocks_keep_assistant_answer_separate_from_activity() {
        let model = ConversationModel {
            items: vec![
                ChatItem::Assistant {
                    text: "final answer".into(),
                },
                ChatItem::ToolCard {
                    name: "read_file".into(),
                    summary: "src/lib.rs · 1 line".into(),
                    detail: "src/lib.rs".into(),
                    state: ToolCardState::Done,
                    duration: None,
                },
            ],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };

        let blocks = model.semantic_blocks();
        assert!(matches!(
            blocks.first(),
            Some(ConversationBlock::AssistantAnswer(_))
        ));
        assert!(matches!(
            blocks.last(),
            Some(ConversationBlock::ActivityGroup(_))
        ));
    }

    #[test]
    fn completed_turn_composes_answer_before_activity() {
        let model = ConversationModel {
            items: vec![
                ChatItem::User {
                    text: "Summarize this codebase".into(),
                },
                ChatItem::ToolCard {
                    name: "read_file".into(),
                    summary: "README.md · 10 lines".into(),
                    detail: "README.md".into(),
                    state: ToolCardState::Done,
                    duration: None,
                },
                ChatItem::ToolCard {
                    name: "fffind".into(),
                    summary: "crate · 3 files".into(),
                    detail: "crates/".into(),
                    state: ToolCardState::Done,
                    duration: None,
                },
                ChatItem::Assistant {
                    text: "Forge is a Rust workspace.".into(),
                },
            ],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };

        let blocks = model.semantic_blocks();
        assert!(matches!(
            blocks.as_slice(),
            [
                ConversationBlock::UserMessage(_),
                ConversationBlock::AssistantAnswer(a),
                ConversationBlock::ActivityGroup(_),
            ] if a.text == "Forge is a Rust workspace."
        ));
    }

    #[test]
    fn thinking_is_never_promoted_to_final_answer() {
        let messages = vec![
            Message {
                role: MessageRole::User,
                content: "Summarize this codebase".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                role: MessageRole::Assistant,
                content: String::new(),
                tool_call_id: None,
                name: None,
                thinking: Some("First, the user asked to summarize...".into()),
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
        ];
        let model = ConversationModel::from_messages(
            &messages,
            &[],
            TaskLifecycle::Completed,
            ConversationViewOpts::default(),
        );
        let blocks = model.semantic_blocks();
        assert!(
            !blocks
                .iter()
                .any(|b| matches!(b, ConversationBlock::AssistantAnswer(_))),
            "thinking-only assistant must not become answer: {blocks:?}"
        );
    }

    #[test]
    fn sanitize_strips_confidence_protocol_marker() {
        assert_eq!(
            sanitize_final_answer_text("Forge is a workspace.\\confidence{80}"),
            "Forge is a workspace."
        );
        assert_eq!(sanitize_final_answer_text("answer only"), "answer only");
    }

    #[test]
    fn failed_turn_renders_failure_before_activity() {
        let model = ConversationModel {
            items: vec![
                ChatItem::User {
                    text: "Summarize this codebase".into(),
                },
                ChatItem::ToolCard {
                    name: "read_file".into(),
                    summary: "invalid arguments · retry 1".into(),
                    detail: "offset type mismatch".into(),
                    state: ToolCardState::Error,
                    duration: None,
                },
                ChatItem::Banner {
                    text: "Forge couldn't complete this turn after repeated invalid tool calls."
                        .into(),
                    kind: BannerKind::Error,
                },
            ],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let blocks = model.semantic_blocks();
        assert!(matches!(
            blocks.as_slice(),
            [
                ConversationBlock::UserMessage(_),
                ConversationBlock::Callout(c),
                ConversationBlock::ActivityGroup(_),
            ] if matches!(c.kind, BannerKind::Error)
                && c.text.contains("couldn't complete")
        ));
    }

    #[test]
    fn turn_failed_marker_is_not_an_assistant_answer() {
        let messages = vec![
            Message {
                role: MessageRole::User,
                content: "Summarize this codebase".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                role: MessageRole::Assistant,
                content: format!("{TURN_FAILED_MARKER}Forge couldn't complete this turn."),
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
            TaskLifecycle::Failed,
            ConversationViewOpts::default(),
        );
        let blocks = model.semantic_blocks();
        assert!(
            !blocks
                .iter()
                .any(|b| matches!(b, ConversationBlock::AssistantAnswer(_))),
            "failure marker must not render as answer: {blocks:?}"
        );
        assert!(blocks.iter().any(|b| matches!(
            b,
            ConversationBlock::Callout(c)
                if matches!(c.kind, BannerKind::Error)
                    && c.text.contains("couldn't complete")
        )));
    }

    #[test]
    fn only_last_final_answer_per_turn_is_kept() {
        let model = ConversationModel {
            items: vec![
                ChatItem::User { text: "hi".into() },
                ChatItem::Assistant {
                    text: "I need to summarize...".into(),
                },
                ChatItem::Assistant {
                    text: "Forge is a Rust workspace.".into(),
                },
            ],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let blocks = model.semantic_blocks();
        let answers: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                ConversationBlock::AssistantAnswer(a) => Some(a.text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(answers, vec!["Forge is a Rust workspace."]);
    }

    #[test]
    fn legacy_sessions_render_through_the_adapter_without_migration() {
        let messages = vec![
            Message {
                role: MessageRole::User,
                content: "hello".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                role: MessageRole::Assistant,
                content: "hi".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
        ];
        let model = ConversationModel::from_messages(
            &messages,
            &[TurnEvent {
                kind: "legacy".into(),
                detail: "ignored".into(),
            }],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        let blocks = model.semantic_blocks();
        assert!(matches!(
            blocks.as_slice(),
            [
                ConversationBlock::UserMessage(_),
                ConversationBlock::AssistantAnswer(_)
            ]
        ));
    }

    #[test]
    fn expansion_state_does_not_mutate_transcript_data() {
        let collapsed = ConversationModel {
            items: vec![ChatItem::ToolCard {
                name: "bash".into(),
                summary: "$ cargo test · failed".into(),
                detail: "status 101".into(),
                state: ToolCardState::Error,
                duration: None,
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let expanded = ConversationModel {
            opts: ConversationViewOpts {
                tool_expanded: true,
                ..Default::default()
            },
            ..collapsed.clone()
        };

        assert_eq!(collapsed.items, expanded.items);
        assert_ne!(collapsed.lines(), expanded.lines());
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
            TaskLifecycle::Working,
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
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );

        let answer_lines = model
            .lines_for_width(140)
            .iter()
            .filter(|line| line.spans.iter().any(|span| span.content.contains("word")))
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
            TaskLifecycle::Working,
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
            TaskLifecycle::Working,
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
    fn user_messages_render_with_continuous_gutter() {
        const WIDTH: usize = 100;
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
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        let glyph = crate::user_message_gutter::gutter_glyph(&crate::theme::active(), false);
        let lines = m.lines_for_width(WIDTH);
        let rendered_lines = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let rendered = rendered_lines.join("\n");
        assert!(
            rendered_lines[0].ends_with(&format!("{glyph} hello world")),
            "{rendered}"
        );
        assert_eq!(lines[0].width(), WIDTH);
        let dark = theme::palette(forge_config::THEME_SOLARIZED_DARK);
        let first = lines.into_iter().next().expect("operator turn");
        assert_eq!(first.style.bg, Some(dark.user_bg));
        assert_eq!(first.spans[1].style.fg, Some(dark.user_message_gutter));
        assert_eq!(first.spans[3].style.fg, Some(dark.text));
        assert!(!rendered.contains('›'), "{rendered}");
        assert!(rendered.contains("hello world"), "{rendered}");
    }

    #[test]
    fn empty_transcript_renders_without_initial_marker() {
        let model = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Working,
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
            TaskLifecycle::Working,
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
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        assert!(matches!(
            m.items.first(),
            Some(ChatItem::ToolCard {
                name,
                state: ToolCardState::Error,
                ..
            }) if name == "read_file"
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
            TaskLifecycle::Working,
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

        let blocks = model.semantic_blocks();
        assert!(blocks.iter().any(|block| matches!(
            block,
            ConversationBlock::ActivityGroup(group)
                if !group.items.is_empty() && !group.label.is_empty()
        )));

        let expanded = ConversationModel::from_messages(
            &messages,
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts {
                tool_expanded: true,
                ..ConversationViewOpts::default()
            },
        )
        .semantic_blocks();
        assert!(expanded.iter().any(|block| matches!(
            block,
            ConversationBlock::ActivityGroup(group)
                if group.items.iter().any(|item| item.contains("https://ratatui.rs"))
        )));
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

        let blocks = model.semantic_blocks();
        assert!(blocks.iter().any(|block| matches!(
            block,
            ConversationBlock::ActivityGroup(group)
                if !group.items.is_empty()
                    && matches!(group.outcome, ActivityOutcome::Neutral | ActivityOutcome::Success)
        )));
        assert!(blocks
            .iter()
            .any(|block| matches!(block, ConversationBlock::UserMessage(_))));
        assert!(blocks.iter().any(|block| matches!(
            block,
            ConversationBlock::ActivityGroup(group)
                if group.label == "bash" || group.label == "Validation completed"
        )));
    }

    #[test]
    fn grouped_activity_renders_running_completed_and_details() {
        let running = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Working,
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
        let blocks = completed.semantic_blocks();
        assert!(blocks.iter().any(|block| matches!(
            block,
            ConversationBlock::ActivityGroup(group)
                if !group.items.is_empty() && group.count_label.contains("2")
        )));
        assert_eq!(activity_detail_label(true), "  · Ctrl+O collapse");
        assert_eq!(activity_detail_label(false), "  · Ctrl+O details");
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
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        assert_eq!(model.items.len(), 3);
        assert!(matches!(
            model.items[0],
            ChatItem::ToolCard {
                state: ToolCardState::Error,
                ..
            }
        ));
        assert!(matches!(
            model.items[1],
            ChatItem::ToolCard {
                state: ToolCardState::Error,
                summary: ref s,
                ..
            } if s.contains("retry 2")
        ));
        assert!(matches!(model.items[2], ChatItem::RetryAssistant { .. }));
        let blocks = model.semantic_blocks();
        // Validator detail stays in activity evidence; retry assistant is a callout.
        assert!(blocks
            .iter()
            .any(|block| matches!(block, ConversationBlock::ActivityGroup(_))));
        assert!(blocks.iter().any(|block| matches!(
            block,
            ConversationBlock::Callout(_) | ConversationBlock::AssistantAnswer(_)
        )));
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
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        assert!(matches!(m.items[0], ChatItem::DiffCard { .. }));
        let blocks = m.semantic_blocks();
        assert!(matches!(
            blocks.as_slice(),
            [ConversationBlock::DiffBlock(_)]
        ));
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
            TaskLifecycle::Working,
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

        let dark = theme::palette(forge_config::THEME_SOLARIZED_DARK);
        assert!(removed
            .spans
            .iter()
            .all(|span| span.style.bg == Some(dark.diff_remove)));
        assert!(added
            .spans
            .iter()
            .all(|span| span.style.bg == Some(dark.diff_add)));
        assert!(
            added.spans.len() > 3,
            "Rust tokens should be separate spans"
        );
        assert_eq!(
            added.spans.iter().map(|span| span.width()).sum::<usize>(),
            40
        );
    }

    /// Text of every rendered line, for asserting on content rather than styling.
    fn lines_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A streaming answer arrives without its closing fence for as long as the
    /// model is still writing the block. The body must still render: the opening
    /// fence is already on screen, so dropping it shows an empty code block.
    #[test]
    fn unterminated_fence_still_renders_its_code() {
        let streaming = "Here is the function:\n\n```rust\npub fn alpha() -> usize { 41 }\npub fn beta() -> usize { 42 }";

        let text = lines_text(&assistant_lines(streaming, 80));

        assert!(
            text.contains("```rust"),
            "opening fence should render:\n{text}"
        );
        assert!(
            text.contains("alpha"),
            "partial code should render:\n{text}"
        );
        assert!(text.contains("beta"), "partial code should render:\n{text}");
        assert!(
            !text.contains("  ```\n") && !text.ends_with("  ```"),
            "no closing fence should be invented for an unterminated block:\n{text}"
        );
    }

    /// The partial block is highlighted, not dumped as plain text, so it does not
    /// visibly restyle itself when the closing fence finally arrives.
    #[test]
    fn unterminated_fence_is_highlighted_like_a_closed_one() {
        let open = "```rust\npub fn alpha() -> usize { 41 }";
        let closed = "```rust\npub fn alpha() -> usize { 41 }\n```";

        let open_lines = assistant_lines(open, 80);
        let closed_lines = assistant_lines(closed, 80);

        // Find the code line in each rendering and compare span structure.
        let code_of = |lines: &[Line<'static>]| {
            lines
                .iter()
                .find(|l| lines_text(std::slice::from_ref(*l)).contains("alpha"))
                .expect("code line present")
                .clone()
        };
        let open_code = code_of(&open_lines);
        let closed_code = code_of(&closed_lines);

        assert!(
            open_code.spans.len() > 1,
            "partial code should be tokenised, not one plain span: {:?}",
            open_code.spans
        );
        assert_eq!(
            open_code.spans.len(),
            closed_code.spans.len(),
            "partial and closed blocks should highlight identically"
        );
    }

    /// Regression guard for the original defect shape: a message whose only
    /// content is an unterminated block must not render as an empty block.
    #[test]
    fn unterminated_fence_is_not_an_empty_block() {
        let text = lines_text(&assistant_lines("```rust\nlet x = 1;", 80));

        assert!(
            text.contains("let x = 1;"),
            "an unterminated block must not render empty:\n{text}"
        );
    }

    #[test]
    fn empty_shows_blank_conversation() {
        let m = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Working,
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
            TaskLifecycle::Working,
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
            TaskLifecycle::Working,
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
            TaskLifecycle::Working,
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
        // Streaming final-answer deltas render as answer text, not progress.
        assert!(!text.contains("Current progress"));
        assert!(text.contains("partial response▌"));
    }

    #[test]
    fn running_tool_card_shows_intent_without_arguments() {
        let m = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Working,
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
            TaskLifecycle::Waiting,
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
        assert!(!text.contains("git push -u origin feature"));
        assert!(!text.contains("sk-"));
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
        let blocks = m.semantic_blocks();
        assert!(blocks.iter().any(|block| matches!(
            block,
            ConversationBlock::Callout(CalloutPresentation { text, .. })
                if text.contains("rate limiting middleware")
        )));
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
        let blocks = m.semantic_blocks();
        assert!(blocks.iter().any(|block| matches!(
            block,
            ConversationBlock::Callout(CalloutPresentation { text, .. })
                if text.contains("Restoring session")
        )));
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
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        let text = model
            .lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("SENSOR · DETERMINISTIC"), "{text}");
        assert!(
            text.contains("Moving the layer onto the public router."),
            "{text}"
        );
    }

    #[test]
    fn scroll_unpins_follow() {
        let mut m = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Working,
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
