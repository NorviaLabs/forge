//! Conversation view model (TUI-02) — polished chat, thinking, tools, diffs.

use crate::markdown::render_markdown;
use crate::status_glyph::{status_glyph, Status};
use crate::theme;
use crate::user_message_gutter;
use forge_core::{AgentSession, TurnEvent, TURN_FAILED_MARKER};
use forge_syntax::highlight_to_lines;
use forge_types::{ExecutionOutcome, Message, MessageRole, TaskLifecycle, ToolCall};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Widget};

const DIFF_BLOCK_MARKER: &str = "\u{200b}";
const DIFF_BLOCK_END_MARKER: &str = "\u{200c}";
const INDENT_UNIT: &str = "  ";
const MESSAGE_PADDING: usize = 2;
const PROSE_MAX_WIDTH: usize = 72;
/// Subtle left rail grouping tool calls and progress under the current turn.
const RAIL_GLYPH: &str = "│";
/// Pane widths below this drop the rail and indent (flat mode).
const RAIL_MIN_WIDTH: usize = 50;
/// Columns the rail unit (`│ `) consumes from wrapped content.
const RAIL_EXTRA: usize = 2;

/// Blocks rendered on the turn's tool rail: grouped under the turn, compact,
/// and subordinate to user turns, phases, gates, and the final answer.
fn is_railed_block(block: &ConversationBlock) -> bool {
    matches!(
        block,
        ConversationBlock::ActivityGroup(_) | ConversationBlock::ActiveProgress(_)
    )
}

/// Add a blank separator line unless the last line is already blank.
fn ensure_blank_line(lines: &mut Vec<Line<'static>>) {
    let last_blank = lines
        .last()
        .is_none_or(|l| l.spans.iter().all(|s| s.content.is_empty()));
    if !last_blank {
        lines.push(Line::from(""));
    }
}

/// A bordered card's top edge: `┌─ {title} ───┐` when `title` is set
/// (Approval), or a plain `┌────┐` when it isn't (Plan). `total_width` is the
/// full rendered line width (the card's content width plus its 2 side
/// borders and 2 padding columns).
fn card_top_border(total_width: usize, title: Option<&str>, border: Style) -> Line<'static> {
    match title {
        Some(title) => {
            let fill = total_width
                .saturating_sub(5)
                .saturating_sub(title.chars().count());
            Line::from(vec![Span::styled(
                format!("┌─ {title} {}┐", "─".repeat(fill)),
                border,
            )])
        }
        None => Line::from(vec![Span::styled(
            format!("┌{}┐", "─".repeat(total_width.saturating_sub(2))),
            border,
        )]),
    }
}

/// A bordered card's bottom edge: `└────┘`.
fn card_bottom_border(total_width: usize, border: Style) -> Line<'static> {
    Line::from(vec![Span::styled(
        format!("└{}┘", "─".repeat(total_width.saturating_sub(2))),
        border,
    )])
}

/// A bordered card's content row: `│ {content, padded to interior_width} │`.
/// `fill`, when set, paints the row's background edge-to-edge (Approval
/// wants `panel_alt`; Plan wants none — canvas shows through).
fn card_content_line(
    content: &str,
    interior_width: usize,
    style: Style,
    border: Style,
    fill: Option<Color>,
) -> Line<'static> {
    let pad = " ".repeat(interior_width.saturating_sub(content.chars().count()));
    let border_style = match fill {
        Some(bg) => border.bg(bg),
        None => border,
    };
    let content_style = match fill {
        Some(bg) => style.bg(bg),
        None => style,
    };
    Line::from(vec![
        Span::styled("│ ", border_style),
        Span::styled(format!("{content}{pad}"), content_style),
        Span::styled(" │", border_style),
    ])
}

/// Like [`card_content_line`], but for a row built from several differently
/// styled spans (e.g. a colored status marker followed by plain body text)
/// instead of one uniformly styled string.
fn card_content_spans(
    mut spans: Vec<Span<'static>>,
    interior_width: usize,
    border: Style,
    fill: Option<Color>,
) -> Line<'static> {
    let used: usize = spans.iter().map(Span::width).sum();
    let pad = " ".repeat(interior_width.saturating_sub(used));
    if let Some(bg) = fill {
        for span in &mut spans {
            span.style = span.style.bg(bg);
        }
    }
    let border_style = match fill {
        Some(bg) => border.bg(bg),
        None => border,
    };
    let mut line_spans = vec![Span::styled("│ ", border_style)];
    line_spans.append(&mut spans);
    line_spans.push(Span::styled(
        pad,
        fill.map_or(Style::default(), |bg| Style::default().bg(bg)),
    ));
    line_spans.push(Span::styled(" │", border_style));
    Line::from(line_spans)
}

/// Prepend a rail glyph in the given style to a rendered line.
fn prefix_line_with(line: &mut Line<'static>, glyph_style: Style) {
    let mut spans = vec![Span::styled(RAIL_GLYPH, glyph_style), Span::raw(" ")];
    spans.extend(std::mem::take(&mut line.spans));
    line.spans = spans;
}

/// Prepend the left-rail glyph to a rendered line.
fn prefix_line_rail(line: &mut Line<'static>) {
    prefix_line_with(line, theme::border_muted());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCardState {
    Running,
    Done,
    Blocked,
    Error,
}

impl From<&ExecutionOutcome> for ToolCardState {
    /// The coarse Running/Blocked/Done/Error bucket only needs to know
    /// success vs. not — the precise variant (Denied vs. Cancelled vs.
    /// TimedOut) lives on `ActivityOutcome`/the rendered copy, not here.
    /// Denied is deliberately `Error`, not `Blocked`: it's terminal and
    /// negative, not pending (see `ExecutionOutcome::Denied` docs).
    fn from(outcome: &ExecutionOutcome) -> Self {
        match outcome {
            ExecutionOutcome::Success => ToolCardState::Done,
            ExecutionOutcome::Failed { .. }
            | ExecutionOutcome::SpawnFailed { .. }
            | ExecutionOutcome::Denied { .. }
            | ExecutionOutcome::Cancelled
            | ExecutionOutcome::TimedOut => ToolCardState::Error,
            // `ExecutionOutcome` is `#[non_exhaustive]`; an outcome this
            // build doesn't recognise must never read as `Done`.
            _ => ToolCardState::Error,
        }
    }
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
            // Deliberately outcome-neutral: whether this passed or failed is
            // never asserted here. The concrete pass/fail copy (e.g. "Tests
            // passed" / "Tests failed · exit code 101") comes from
            // `activity_group_summary`, which has the real outcome.
            (Self::Validating, false) => "Validation",
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
        /// Always-visible invocation line under the tool label (e.g.
        /// `git status --short`, `src/main.rs`). `None` for render-only tools.
        subcommand: Option<String>,
        /// Real execution result. `ExecutionOutcome::Success` for tools with
        /// no real process/failure concept (e.g. `read_file`, `fffind`).
        outcome: forge_types::ExecutionOutcome,
    },
    ActivityGroup {
        category: ActivityCategory,
        summary: String,
        detail: String,
        state: ToolCardState,
        /// Aggregate outcome across the group's members: `Failed` if any
        /// member failed, else `Success`. Drives icon/color and lets a
        /// failing validation command stay grouped instead of falling out
        /// to a standalone card.
        outcome: forge_types::ExecutionOutcome,
    },
    /// Unified-ish diff snippet for write tools.
    DiffCard {
        path: String,
        lines: Vec<String>,
        /// Brief operator-facing explanation for the change.
        rationale: String,
    },
    /// Pending human-in-the-loop approval — the full redacted payload,
    /// rendered inline in the transcript until the composer resolves it.
    ApprovalPending(ApprovalPendingPresentation),
    /// Structured TODO checklist from the `update_plan` tool.
    PlanChecklist {
        explanation: Option<String>,
        steps: Vec<forge_types::PlanItem>,
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
    ApprovalPending(ApprovalPendingPresentation),
    PlanChecklist(PlanChecklistPresentation),
    Metadata(MetadataPresentation),
    Thinking(ThinkingPresentation),
}

/// Model reasoning. Recedes rather than announces: dim italic, indented past
/// tool activity, no glyph or status word — distinct from `ActiveProgress`
/// (which still owns in-flight tool-call status).
#[derive(Debug, Clone, PartialEq)]
pub struct ThinkingPresentation {
    pub text: String,
    pub duration_secs: Option<f64>,
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
    /// Always-visible invocation lines under the label (0-1 per tool call
    /// today; a grouped routine call may later fan out to several).
    pub subcommands: Vec<String>,
    pub items: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityOutcome {
    Success,
    Neutral,
    Warning,
    Failure,
    Blocked,
    Denied,
    Cancelled,
    TimedOut,
}

impl From<&ExecutionOutcome> for ActivityOutcome {
    fn from(outcome: &ExecutionOutcome) -> Self {
        match outcome {
            ExecutionOutcome::Success => ActivityOutcome::Success,
            ExecutionOutcome::Failed { .. } | ExecutionOutcome::SpawnFailed { .. } => {
                ActivityOutcome::Failure
            }
            ExecutionOutcome::Denied { .. } => ActivityOutcome::Denied,
            ExecutionOutcome::Cancelled => ActivityOutcome::Cancelled,
            ExecutionOutcome::TimedOut => ActivityOutcome::TimedOut,
            // `ExecutionOutcome` is `#[non_exhaustive]`; an outcome this
            // build doesn't recognise must never read as `Success`.
            _ => ActivityOutcome::Failure,
        }
    }
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

/// An approval request reduced to what the transcript displays: the command
/// line, the directory it would run in, and any environment delta.
///
/// Turning a `HitlPayload` into these strings means knowing per-tool
/// execution modes, which is the approval overlay's job. The transcript
/// takes the result so it does not have to reach back into overlay state to
/// render a card.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApprovalRequestView {
    pub tool: String,
    pub command: String,
    pub cwd: String,
    pub env_delta: String,
}

/// One selectable row on the inline approval menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalMenuRow {
    pub label: String,
    pub detail: Option<String>,
}

/// The redacted command awaiting a human approval decision, with enough
/// context to tell what would run. Resolution is menu-only; the composer is
/// not an answer input.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalPendingPresentation {
    pub tool: String,
    pub command: String,
    pub cwd: String,
    pub env_delta: String,
    pub options: Vec<ApprovalMenuRow>,
    pub selected: usize,
    /// Whether the approval card itself holds focus (accent border vs muted).
    pub focused: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanChecklistPresentation {
    pub explanation: Option<String>,
    pub steps: Vec<forge_types::PlanItem>,
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
                            if m.thinking_duration_secs.is_some() {
                                items.push(ChatItem::Thinking {
                                    text: th.clone(),
                                    duration_secs: m.thinking_duration_secs,
                                });
                            }
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
                    // Assistant text is durable primary-channel content. A model may
                    // explain what it is about to do in the same response as a tool
                    // call; that text was visible while streaming but used to vanish
                    // as soon as the step settled because tool-call messages were
                    // filtered out here.
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
                        } else {
                            items.push(ChatItem::Assistant {
                                text: effective_text,
                            });
                        }
                    }
                }
                // Tool results are not shown as chat messages (keeps the transcript clean).
                MessageRole::Tool => {
                    let name = m.name.as_deref().unwrap_or("tool");
                    if name == "update_plan" {
                        let call = m
                            .tool_call_id
                            .as_deref()
                            .and_then(|id| tool_calls.get(id).copied());
                        if let Some(args) = call.and_then(parse_update_plan_args) {
                            items.push(ChatItem::PlanChecklist {
                                explanation: args.explanation,
                                steps: args.plan,
                            });
                            continue;
                        }
                    }
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
                            subcommand: None,
                            outcome: forge_types::ExecutionOutcome::Failed { exit_code: None },
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
                        let (state, summary, invocation, detail) =
                            classify_tool_content(name, &m.content, call, &m.outcome);
                        items.push(ChatItem::ToolCard {
                            name: name.to_string(),
                            summary,
                            detail,
                            state,
                            duration: None,
                            subcommand: invocation,
                            outcome: m.outcome.clone(),
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
                text: "Awaiting approval · ↑↓ select · Enter confirm · Esc cancel".into(),
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
                outcome: ExecutionOutcome::Success,
            });
            return self;
        }
        self.items.push(ChatItem::ToolCard {
            name,
            summary: "journal: tool_intent committed · awaiting result".into(),
            detail: String::new(),
            state: ToolCardState::Running,
            duration: None,
            subcommand: None,
            outcome: ExecutionOutcome::Success,
        });
        self
    }

    /// Append the pending HITL approval as a full inline transcript item.
    /// `working_directory` is the workspace-root fallback when the call
    /// carries no explicit cwd.
    pub fn with_pending_approval(
        mut self,
        request: ApprovalRequestView,
        options: Vec<ApprovalMenuRow>,
        selected: usize,
        focused: bool,
    ) -> Self {
        let selected = if options.is_empty() {
            0
        } else {
            selected.min(options.len() - 1)
        };
        self.items
            .push(ChatItem::ApprovalPending(ApprovalPendingPresentation {
                tool: request.tool,
                command: request.command,
                cwd: request.cwd,
                env_delta: request.env_delta,
                options,
                selected,
                focused,
            }));
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
                    | ChatItem::PlanChecklist { .. }
            )
        })
    }

    pub fn lines(&self) -> Vec<Line<'static>> {
        self.lines_for_width(if self.opts.compact { 88 } else { 100 })
    }

    /// Build display lines for the actual conversation viewport. Prose gets a
    /// readable cap; code and structured blocks keep the full pane width.
    pub(crate) fn lines_for_width(&self, available_width: usize) -> Vec<Line<'static>> {
        let width = available_width.max(4);
        let prose_width = width
            .saturating_sub(MESSAGE_PADDING * 2)
            .clamp(4, PROSE_MAX_WIDTH);
        let mut lines = Vec::new();
        let gap = !self.opts.compact;
        let rail = width >= RAIL_MIN_WIDTH;
        let blocks = self.semantic_blocks();
        // A full-width rule opens every turn boundary (every UserMessage
        // after the first block in the transcript) — independent of whether
        // that turn has a plan checklist. Everything on the tool rail stays
        // grouped compactly instead of walled off.
        let mut seen_any_block = false;
        for block in blocks {
            let is_turn_start = matches!(block, ConversationBlock::UserMessage(_));
            let railed = is_railed_block(&block);
            if !railed && gap && !lines.is_empty() {
                // Major blocks read as boundaries: separate them from the
                // preceding tool trail with a single blank line.
                ensure_blank_line(&mut lines);
            }
            if is_turn_start && seen_any_block {
                if gap {
                    ensure_blank_line(&mut lines);
                }
                lines.push(Line::from(Span::styled(
                    "─".repeat(width),
                    theme::border_muted(),
                )));
                if gap {
                    lines.extend([Line::from(""), Line::from("")]);
                }
            }
            seen_any_block = true;
            match block {
                ConversationBlock::UserMessage(p) => {
                    let theme_id = crate::theme::active();
                    let prefix_width = MESSAGE_PADDING;
                    let user_lines = user_message_gutter::render_user_message_lines(
                        &p.text,
                        width.saturating_sub(prefix_width),
                        &theme_id,
                        false,
                        wrap,
                    );
                    for line in user_lines.into_iter() {
                        // No leading marker — just an indent matching
                        // assistant messages' own left padding, with the
                        // highlighted background carried all the way to the
                        // edge so the block reads as one seamless bar.
                        let mut spans = vec![Span::styled(
                            " ".repeat(prefix_width),
                            theme::text().bg(theme::accent_soft_bg()),
                        )];
                        spans.extend(line.spans.into_iter().map(|mut span| {
                            span.style = span.style.bg(theme::accent_soft_bg());
                            span
                        }));
                        let content_width = spans.iter().map(Span::width).sum::<usize>();
                        if content_width < width {
                            spans.push(Span::styled(
                                " ".repeat(width - content_width),
                                theme::text().bg(theme::accent_soft_bg()),
                            ));
                        }
                        lines.push(Line::from(spans));
                    }
                    if gap {
                        lines.extend([Line::from(""), Line::from("")]);
                    }
                }
                ConversationBlock::AssistantAnswer(p) => {
                    let parts = render_markdown(&p.text, prose_width);
                    for line in parts {
                        let mut spans = vec![Span::raw(" ".repeat(MESSAGE_PADDING))];
                        spans.extend(line.spans);
                        let used = spans.iter().map(Span::width).sum::<usize>();
                        if used < width {
                            spans.push(Span::raw(" ".repeat(width - used)));
                        }
                        lines.push(Line::from(spans).style(theme::assistant_answer_style()));
                    }
                    if gap {
                        lines.extend([Line::from(""), Line::from("")]);
                    }
                }
                ConversationBlock::ActiveProgress(p) => {
                    let label = format!("{} · {}", p.label, p.summary);
                    let prefix = if p.id == "stream" { "▍ " } else { "● " };
                    let mut line = Line::from(vec![
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
                    ]);
                    if rail {
                        prefix_line_rail(&mut line);
                    }
                    lines.push(line);
                }
                ConversationBlock::ActivityGroup(p) => {
                    let (prefix, separator) = match p.outcome {
                        ActivityOutcome::Success => (status_glyph(Status::Success), " "),
                        ActivityOutcome::Failure => (status_glyph(Status::Error), " "),
                        ActivityOutcome::Blocked => (Span::styled("⏸", theme::warn()), " "),
                        ActivityOutcome::Warning => (status_glyph(Status::Warning), ""),
                        ActivityOutcome::Neutral => (Span::styled("●", theme::muted()), " "),
                        ActivityOutcome::Denied => {
                            (Span::styled("⊘", theme::tool_denied_style()), " ")
                        }
                        ActivityOutcome::Cancelled => (Span::styled("■", theme::muted()), " "),
                        ActivityOutcome::TimedOut => {
                            (Span::styled("⧖", theme::tool_timeout_style()), " ")
                        }
                    };
                    let mut spans = vec![
                        prefix,
                        Span::raw(separator),
                        Span::styled(p.label, theme::text().add_modifier(Modifier::BOLD)),
                        Span::styled("  ", theme::metadata_style()),
                    ];
                    if p.subcommands.is_empty() {
                        match collapsed_command_summary(&p.count_label, &p.items) {
                            Some((command, output_lines)) => {
                                spans.push(Span::styled(command, theme::metadata_style()));
                                spans.push(Span::styled(
                                    format!(" · {output_lines} output lines"),
                                    theme::dim(),
                                ));
                            }
                            None => {
                                spans.push(Span::styled(p.count_label, theme::metadata_style()))
                            }
                        }
                    }
                    spans.push(Span::styled(
                        activity_detail_label(p.expanded),
                        theme::metadata_style(),
                    ));
                    let mut line = Line::from(spans);
                    if rail {
                        prefix_line_rail(&mut line);
                    }
                    lines.push(line);
                    let rail_extra = if rail { RAIL_EXTRA } else { 0 };
                    for (index, subcommand) in p.subcommands.iter().enumerate() {
                        let last = index + 1 == p.subcommands.len();
                        let glyph = if last { "└─" } else { "├─" };
                        let sub_width = width.saturating_sub(5 + rail_extra);
                        for (lineno, wrapped) in wrap(subcommand, sub_width).into_iter().enumerate()
                        {
                            let head = if lineno == 0 { glyph } else { "│" };
                            let mut sub_line = Line::from(Span::styled(
                                format!("{INDENT_UNIT}{head} {wrapped}"),
                                theme::muted(),
                            ));
                            if rail {
                                prefix_line_rail(&mut sub_line);
                            }
                            lines.push(sub_line);
                        }
                    }
                    if p.expanded {
                        for item in p.items {
                            for wrapped in wrap(&item, width.saturating_sub(2 + rail_extra)) {
                                let mut item_line = Line::from(Span::styled(
                                    format!("{INDENT_UNIT}{wrapped}"),
                                    theme::muted(),
                                ));
                                if rail {
                                    prefix_line_rail(&mut item_line);
                                }
                                lines.push(item_line);
                            }
                        }
                    }
                }
                ConversationBlock::ApprovalPending(p) => {
                    let border = if p.focused {
                        theme::approval_accent()
                    } else {
                        theme::border_muted()
                    };
                    let title = "⏸ APPROVAL REQUIRED";
                    const HINT: &str = "↑↓ select · Enter confirm · Esc cancel";
                    let cwd_env = format!("cwd: {}  env: {}", p.cwd, p.env_delta);
                    let option_rows: Vec<String> = p
                        .options
                        .iter()
                        .map(|opt| match &opt.detail {
                            Some(detail) => format!("›  {}  {detail}", opt.label),
                            None => format!("›  {}", opt.label),
                        })
                        .collect();
                    // Hug the card's own content instead of always spanning
                    // the full pane width; still capped at prose width for
                    // readability and clamped to what the pane can show.
                    let longest_content = [
                        title.chars().count() + 5,
                        cwd_env.chars().count(),
                        HINT.chars().count(),
                        p.command.chars().count(),
                    ]
                    .into_iter()
                    .chain(option_rows.iter().map(|r| r.chars().count()))
                    .max()
                    .unwrap_or(0);
                    let available_interior = width.saturating_sub(4);
                    let inner_w = longest_content
                        .min(PROSE_MAX_WIDTH)
                        .min(available_interior)
                        .max((title.chars().count() + 1).min(available_interior));
                    let card_width = inner_w + 4;
                    let fill = Some(theme::panel_alt_bg());
                    let boxed_line =
                        |s: &str, style: Style| card_content_line(s, inner_w, style, border, fill);
                    lines.push(card_top_border(card_width, Some(title), border));
                    lines.push(boxed_line("", theme::panel()));
                    for wrapped in wrap(&p.command, inner_w) {
                        lines.push(boxed_line(&wrapped, theme::muted()));
                    }
                    for wrapped in wrap(&cwd_env, inner_w) {
                        lines.push(boxed_line(&wrapped, theme::muted()));
                    }
                    lines.push(boxed_line("", theme::panel()));
                    for (idx, opt) in p.options.iter().enumerate() {
                        let marker = if idx == p.selected { "›" } else { " " };
                        let style = if idx == p.selected {
                            theme::text().add_modifier(Modifier::BOLD)
                        } else {
                            theme::muted()
                        };
                        let row = match &opt.detail {
                            Some(detail) => format!("{marker} {}  {detail}", opt.label),
                            None => format!("{marker} {}", opt.label),
                        };
                        for wrapped in wrap(&row, inner_w) {
                            lines.push(boxed_line(&wrapped, style));
                        }
                    }
                    for wrapped in wrap(HINT, inner_w) {
                        lines.push(boxed_line(&wrapped, theme::metadata_style()));
                    }
                    lines.push(boxed_line("", theme::panel()));
                    lines.push(card_bottom_border(card_width, border));
                    if gap {
                        lines.extend([Line::from(""), Line::from("")]);
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
                        lines.extend([Line::from(""), Line::from("")]);
                    }
                }
                ConversationBlock::CodeBlock(p) => {
                    for line in render_markdown(&p.text, width) {
                        lines.push(line.style(theme::code_block()));
                    }
                    if gap {
                        lines.extend([Line::from(""), Line::from("")]);
                    }
                }
                ConversationBlock::DiffBlock(p) => {
                    lines.push(diff_title_line(&p.path, &p.lines));
                    if !p.rationale.is_empty() {
                        for l in wrap(&p.rationale, width.saturating_sub(6))
                            .into_iter()
                            .take(2)
                        {
                            lines.push(Line::from(vec![
                                Span::styled(INDENT_UNIT, theme::info()),
                                Span::styled(l, theme::muted().add_modifier(Modifier::ITALIC)),
                            ]));
                        }
                    }
                    lines.extend(render_numbered_diff(
                        &p.path,
                        &p.lines,
                        width.saturating_sub(2),
                    ));
                    lines.push(Line::from(DIFF_BLOCK_END_MARKER));
                    if gap {
                        lines.extend([Line::from(""), Line::from("")]);
                    }
                }
                ConversationBlock::PlanChecklist(p) => {
                    lines.extend(render_plan_checklist(&p, width));
                    if gap {
                        lines.extend([Line::from(""), Line::from("")]);
                    }
                }
                ConversationBlock::Metadata(p) => {
                    for l in wrap(&p.text, width) {
                        lines.push(Line::from(Span::styled(l, theme::muted())));
                    }
                    if gap {
                        lines.extend([Line::from(""), Line::from("")]);
                    }
                }
                ConversationBlock::Thinking(p) => {
                    // Recedes rather than announces: no glyph, no bold label,
                    // no status word — deeper-indented and dim so it reads as
                    // background reasoning, not another activity item.
                    let indent = INDENT_UNIT.repeat(2);
                    let content_width = width.saturating_sub(indent.chars().count());
                    let full_text = match p.duration_secs {
                        Some(secs) => format!("{} · {}", p.text, format_elapsed_tenths(secs)),
                        None => p.text.clone(),
                    };
                    for l in wrap(&full_text, content_width) {
                        lines.push(Line::from(Span::styled(
                            format!("{indent}{l}"),
                            theme::dim().add_modifier(Modifier::ITALIC),
                        )));
                    }
                    if gap {
                        lines.extend([Line::from(""), Line::from("")]);
                    }
                }
            }
        }
        lines
    }
}

fn diff_title_line(path: &str, diff: &[String]) -> Line<'static> {
    let numbered = number_diff_lines(diff);
    let additions = numbered.iter().filter(|line| line.marker == '+').count();
    let removals = numbered.iter().filter(|line| line.marker == '-').count();
    Line::from(vec![
        Span::raw(DIFF_BLOCK_MARKER),
        Span::raw(" "),
        Span::styled(path.to_string(), theme::text().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(format!("+{additions}"), theme::ok()),
        Span::raw(" "),
        Span::styled(format!("-{removals}"), theme::danger()),
        Span::raw(" "),
    ])
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
            item.expanded = tool_expanded;
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
            ChatItem::Thinking {
                text,
                duration_secs,
            } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::Thinking(ThinkingPresentation {
                    text: text.clone(),
                    duration_secs: *duration_secs,
                }));
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
                subcommand,
                outcome,
                ..
            } => {
                if let Some(entry) = activity_entry_from_tool(
                    name,
                    summary,
                    detail,
                    *state,
                    outcome,
                    subcommand.as_deref(),
                ) {
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
                                ToolCardState::Error => ActivityOutcome::from(outcome),
                            },
                            expanded: tool_expanded,
                            subcommands: subcommand_line(subcommand.as_deref(), summary),
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
                outcome,
            } => {
                flush_progress(&mut blocks, &mut progress);
                let activity_outcome = match state {
                    ToolCardState::Running => ActivityOutcome::Neutral,
                    // Routine exploration is evidence, not a green "success" banner.
                    ToolCardState::Done if matches!(category, ActivityCategory::Exploring) => {
                        ActivityOutcome::Neutral
                    }
                    ToolCardState::Done => ActivityOutcome::Success,
                    ToolCardState::Blocked => ActivityOutcome::Blocked,
                    ToolCardState::Error => ActivityOutcome::from(outcome),
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
                        outcome: activity_outcome,
                        expanded: tool_expanded,
                        subcommands: Vec::new(),
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
            ChatItem::ApprovalPending(presentation) => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::ApprovalPending(presentation.clone()));
            }
            ChatItem::PlanChecklist { explanation, steps } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::PlanChecklist(
                    PlanChecklistPresentation {
                        explanation: explanation.clone(),
                        steps: steps.clone(),
                    },
                ));
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
/// UserMessage → ActivityGroup → AssistantAnswer(s)|TurnFailure, so the
/// transcript retains model narration around tool work. ActiveProgress is
/// kept only while no terminal answer/failure exists yet.
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
                ConversationBlock::ActivityGroup(_)
                | ConversationBlock::DiffBlock(_)
                | ConversationBlock::PlanChecklist(_)
                | ConversationBlock::Thinking(_) => activity.push(block),
                ConversationBlock::ActiveProgress(_) => progress.push(block),
                other_block => other.push(other_block),
            }
        }
        // Consecutive streaming previews are snapshots of one answer, so keep
        // only the newest preview. Durable assistant messages are separate
        // model steps and must all remain visible around tool activity.
        if answers.len() > 1
            && answers.iter().all(|block| {
                matches!(
                    block,
                    ConversationBlock::AssistantAnswer(answer) if answer.streaming
                )
            })
        {
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
        // Chronological: tool activity ran before the final answer/failure.
        out.extend(activity);
        out.extend(answers);
        out.extend(failures);
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
    execution_outcome: &ExecutionOutcome,
    subcommand: Option<&str>,
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
        // Precise variant (Denied/Cancelled/TimedOut/plain failure) rather
        // than a blanket `Failure`, so a denied/cancelled/timed-out
        // validation command gets its own amber icon, not red.
        ToolCardState::Error => ActivityOutcome::from(execution_outcome),
    };
    let label = match state {
        ToolCardState::Running => category.label(true).to_string(),
        _ => category.label(false).to_string(),
    };
    // Validation commands keep their pass/fail phrase (never re-derived from
    // rendered text) on the always-visible subcommand line.
    let subcommands = if category == ActivityCategory::Validating {
        match subcommand {
            Some(invocation) => vec![format!(
                "{invocation} · {}",
                validation_outcome_summary(execution_outcome)
            )],
            None => Vec::new(),
        }
    } else {
        subcommand_line(subcommand, summary)
    };
    Some(ActivityGroupPresentation {
        id: format!("activity:{category:?}"),
        label,
        count_label: if matches!(state, ToolCardState::Running) {
            running_activity_summary(category, name)
        } else if category == ActivityCategory::Validating {
            // Keep the command visible (so the operator can see what ran)
            // but the pass/fail phrase always comes from the real outcome —
            // never re-derived from rendered text.
            let command = summary.split(" · ").next().unwrap_or(summary);
            format!(
                "{command} · {}",
                validation_outcome_summary(execution_outcome)
            )
        } else {
            result_count_label(1, "item", "items")
        },
        outcome,
        expanded: matches!(state, ToolCardState::Error),
        subcommands,
        items: vec![format!("{name}: {summary}\n{detail}")],
    })
}

/// Always-visible invocation line(s) under a tool label. When the fused
/// summary already leads with the invocation (e.g. `git status --short ·
/// 12 output lines`) the whole summary is kept so counts ride along;
/// otherwise just the invocation is shown (e.g. write tools, whose summary
/// is an output preview, not the path).
fn subcommand_line(invocation: Option<&str>, summary: &str) -> Vec<String> {
    match invocation {
        Some(invocation) if summary.starts_with(invocation) => vec![summary.to_string()],
        Some(invocation) => vec![invocation.to_string()],
        None => Vec::new(),
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
        "  [Ctrl + o] collapse"
    } else {
        "  [Ctrl + o]"
    }
}

/// Matches the truncation length already used for long single-line summaries
/// elsewhere in this file (see the `wrote ·` write/edit summary above).
const COMMAND_LINE_MAX_CHARS: usize = 80;

/// Collapsed-line rendering for command-execution activity groups (see
/// [`activity_entry_from_tool`] and the `ChatItem::ActivityGroup` case in
/// [`semantic_blocks_from_items`], both of which set `count_label` to the
/// raw `"$ command"` text for validation/command entries).
///
/// Returns `Some((truncated_command, output_line_count))` when `count_label`
/// is a command line that exceeds [`COMMAND_LINE_MAX_CHARS`]; `None` leaves
/// short commands and non-command summaries (file counts, etc.) untouched so
/// the caller falls back to rendering `count_label` as-is.
fn collapsed_command_summary(count_label: &str, items: &[String]) -> Option<(String, usize)> {
    if count_label.chars().count() <= COMMAND_LINE_MAX_CHARS {
        return None;
    }
    let command = count_label.strip_prefix("$ ")?;
    let segment = first_command_segment(command);
    let mut truncated: String = segment.chars().take(COMMAND_LINE_MAX_CHARS).collect();
    if segment.chars().count() > COMMAND_LINE_MAX_CHARS {
        truncated.push('…');
    }
    let output_lines: usize = items.iter().map(|item| item.lines().count()).sum();
    Some((format!("$ {truncated}"), output_lines))
}

/// First command/pipe segment of a (possibly chained) shell command line,
/// splitting at the earliest `;`, `&&`, or `|`.
fn first_command_segment(command: &str) -> &str {
    let mut end = command.len();
    for sep in [";", "&&", "|"] {
        if let Some(idx) = command.find(sep) {
            end = end.min(idx);
        }
    }
    command[..end].trim_end()
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
            let text = line.content;
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
                let mut style = theme::syntax_segment(
                    *rgb,
                    Some(line_style.bg.unwrap_or(theme::panel_alt_bg())),
                );
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

/// Aggregate outcome across a group's members: the first non-`Success`
/// outcome found, else `Success`. Drives the group's own state/color so a
/// failing validation command turns the whole "Validating" group
/// Failure-colored instead of silently reading as green.
fn aggregate_outcome(items: &[ChatItem]) -> ExecutionOutcome {
    items
        .iter()
        .filter_map(|item| match item {
            ChatItem::ToolCard { outcome, .. } | ChatItem::ActivityGroup { outcome, .. } => {
                Some(outcome.clone())
            }
            _ => None,
        })
        .find(|o| !o.is_success())
        .unwrap_or(ExecutionOutcome::Success)
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
    let outcome = aggregate_outcome(pending);
    let state = ToolCardState::from(&outcome);
    grouped.push(ChatItem::ActivityGroup {
        category,
        summary,
        detail,
        state,
        outcome,
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
        // A failing *validation* command still groups under "Validating"
        // (the group turns Failure-colored) rather than falling out to a
        // standalone card — other failed tools stand alone so a failure is
        // never buried inside what would otherwise read as a routine
        // "Exploring"/"Implementing" banner.
        ChatItem::ToolCard {
            name,
            summary,
            state: ToolCardState::Error,
            ..
        } => match routine_tool_category(name, summary, None) {
            Some(ActivityCategory::Validating) => Some(ActivityCategory::Validating),
            _ => None,
        },
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
        ActivityCategory::Validating => {
            let command = items
                .iter()
                .filter_map(|item| match item {
                    ChatItem::ToolCard { summary, .. } => summary.split(" · ").next(),
                    _ => None,
                })
                .next();
            let phrase = validation_outcome_summary(&aggregate_outcome(items));
            match command {
                Some(command) => format!("{command} · {phrase}"),
                None => phrase,
            }
        }
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
        ChatItem::PlanChecklist { explanation, steps } => {
            let mut out = String::from("plan");
            if let Some(explanation) = explanation {
                out.push_str(": ");
                out.push_str(explanation);
            }
            for step in steps {
                out.push('\n');
                out.push_str(step.status.as_str());
                out.push_str(" · ");
                out.push_str(&step.step);
            }
            out
        }
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

fn parse_update_plan_args(call: &ToolCall) -> Option<forge_types::UpdatePlanArgs> {
    serde_json::from_value(call.arguments.clone()).ok()
}

fn render_plan_checklist(plan: &PlanChecklistPresentation, width: usize) -> Vec<Line<'static>> {
    use forge_types::PlanStepStatus;
    let mut lines = Vec::new();
    let explanation = plan
        .explanation
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(explanation) = explanation {
        for l in wrap(explanation, width.saturating_sub(2)) {
            lines.push(Line::from(vec![
                Span::raw(INDENT_UNIT),
                Span::styled(l, theme::muted().add_modifier(Modifier::ITALIC)),
            ]));
        }
    }
    // A bordered, unfilled card — no "Plan" caption. The border itself
    // signals "this is a checklist"; [✓]/[►]/[ ] checkboxes signal step status.
    let longest_content = plan
        .steps
        .iter()
        .map(|item| item.step.chars().count() + 4) // checkbox + space
        .chain(explanation.map(|e| e.chars().count()))
        .max()
        .unwrap_or(0);
    let available_interior = width.saturating_sub(4);
    let inner_w = longest_content.min(PROSE_MAX_WIDTH).min(available_interior);
    let border = theme::accent_style();
    lines.push(card_top_border(inner_w + 4, None, border));
    for item in &plan.steps {
        let (marker, style) = match item.status {
            PlanStepStatus::Completed => ("[✓]", theme::ok()),
            PlanStepStatus::InProgress => ("[►]", theme::warn()),
            PlanStepStatus::Pending => ("[ ]", theme::muted()),
        };
        let body_width = inner_w.saturating_sub(4).max(4);
        let mut wrapped = wrap(&item.step, body_width).into_iter();
        if let Some(first) = wrapped.next() {
            lines.push(card_content_spans(
                vec![
                    Span::styled(format!("{marker} "), style),
                    Span::styled(first, theme::text()),
                ],
                inner_w,
                border,
                None,
            ));
        }
        for cont in wrapped {
            lines.push(card_content_spans(
                vec![Span::raw("    "), Span::styled(cont, theme::text())],
                inner_w,
                border,
                None,
            ));
        }
    }
    lines.push(card_bottom_border(inner_w + 4, border));
    lines
}

/// The per-outcome copy fragment appended to a bash/validation summary,
/// e.g. `$ cargo test · failed · exit code 101`. Never derived from
/// substring-matching rendered output — always from the real `outcome`.
fn outcome_label(outcome: &forge_types::ExecutionOutcome, count: usize) -> String {
    use forge_types::ExecutionOutcome;
    match outcome {
        ExecutionOutcome::Success if count == 0 => "completed".to_string(),
        ExecutionOutcome::Success => result_count_label(count, "output line", "output lines"),
        ExecutionOutcome::Failed {
            exit_code: Some(code),
        } => format!("failed · exit code {code}"),
        ExecutionOutcome::Failed { exit_code: None } => "failed".to_string(),
        ExecutionOutcome::SpawnFailed { .. } => "failed · command not found".to_string(),
        ExecutionOutcome::Denied { .. } => "skipped · denied".to_string(),
        ExecutionOutcome::Cancelled => "cancelled".to_string(),
        ExecutionOutcome::TimedOut => "timed out".to_string(),
        // `ExecutionOutcome` is `#[non_exhaustive]`; an outcome this build
        // doesn't recognise must never read as a plain "completed".
        _ => "failed".to_string(),
    }
}

/// The collapsed "Validating" group's pass/fail line, e.g. "Tests passed" /
/// "Tests failed · exit code 101" / "Validation skipped · denied". Always
/// derived from the real aggregate `ExecutionOutcome` — never from the
/// category's own (outcome-neutral) static label.
fn validation_outcome_summary(outcome: &ExecutionOutcome) -> String {
    match outcome {
        ExecutionOutcome::Success => "Tests passed".to_string(),
        ExecutionOutcome::Failed {
            exit_code: Some(code),
        } => format!("Tests failed · exit code {code}"),
        ExecutionOutcome::Failed { exit_code: None } => "Tests failed".to_string(),
        ExecutionOutcome::SpawnFailed { .. } => "Tests failed · command not found".to_string(),
        ExecutionOutcome::Denied { .. } => "Validation skipped · denied".to_string(),
        ExecutionOutcome::Cancelled => "Validation cancelled".to_string(),
        ExecutionOutcome::TimedOut => "Validation timed out".to_string(),
        // `ExecutionOutcome` is `#[non_exhaustive]`; an outcome this build
        // doesn't recognise must never read as "Tests passed".
        _ => "Validation failed".to_string(),
    }
}

fn classify_tool_content(
    name: &str,
    content: &str,
    call: Option<&ToolCall>,
    outcome: &forge_types::ExecutionOutcome,
) -> (ToolCardState, String, Option<String>, String) {
    let detail = redact_tool_output(content);
    // Human-readable invocation for the subcommand line under the tool label.
    // Redacted the same way as the command text: a sensitive command must not
    // surface on an always-visible line.
    let invocation = call
        .and_then(|call| forge_tools::tool_invocation(name, &call.arguments))
        .map(|s| redact_tool_output(&s))
        .and_then(|s| (s != "[redacted tool output]").then_some(s));
    if matches!(name, "exec_command" | "write_stdin") {
        if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&detail) {
            let output = payload["output"].as_str().unwrap_or_default();
            let command = payload["command"].as_str();
            let session_id = payload["session_id"].as_u64();
            let running = payload["running"].as_bool().unwrap_or(false);
            let state = if running {
                ToolCardState::Running
            } else {
                ToolCardState::Done
            };
            let label = if running { "running" } else { "exited" };
            let summary = match (command, session_id) {
                (Some(command), Some(id)) => format!("$ {command} · session #{id} · {label}"),
                (Some(command), None) => format!("$ {command} · {label}"),
                (None, Some(id)) => format!("session #{id} · {label}"),
                (None, None) => label.into(),
            };
            return (state, summary, invocation, output.into());
        }
    }
    let lower = detail.to_ascii_lowercase();
    // The real `outcome` is authoritative for pass/fail — never re-derived by
    // pattern-matching rendered text. Only a genuinely pending signal (no
    // terminal outcome yet, e.g. content narrating an in-flight HITL wait)
    // falls back to substring detection, since `ExecutionOutcome` has no
    // "pending" variant of its own.
    let state = if !outcome.is_success() {
        ToolCardState::from(outcome)
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
            let label = outcome_label(outcome, count);
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
    (state, summary, invocation, detail)
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

#[cfg(test)]
pub struct ConversationWidget<'a> {
    pub model: &'a ConversationModel,
}

pub struct ConversationLinesWidget<'a> {
    pub lines: &'a [Line<'static>],
    pub tail_lines: &'a [Line<'static>],
    pub scroll: u16,
    pub follow: bool,
    pub bottom_padding: u16,
}

fn render_conversation_lines(
    lines: &[Line<'static>],
    tail_lines: &[Line<'static>],
    scroll_from_bottom: u16,
    follow: bool,
    bottom_padding: u16,
    area: Rect,
    buf: &mut Buffer,
) {
    theme::fill(area, buf, theme::assistant_message());
    let content_len = lines.len().saturating_add(tail_lines.len());
    let total = content_len.saturating_add(bottom_padding as usize);
    let max_scroll = total.saturating_sub(area.height as usize);
    let scroll = if follow {
        max_scroll
    } else {
        max_scroll.saturating_sub((scroll_from_bottom as usize).min(max_scroll))
    };
    let end = scroll.saturating_add(area.height as usize).min(total);
    // Borrowed, not cloned: these lines come from the render cache and are
    // reused every frame. Deep-copying each visible one (and every owned string
    // inside its spans) was pure per-frame waste.
    let blank = Line::from("");
    let visible = (scroll..end)
        .map(|index| {
            if index < lines.len() {
                &lines[index]
            } else if index < content_len {
                &tail_lines[index - lines.len()]
            } else {
                &blank
            }
        })
        .collect::<Vec<_>>();
    render_visible_conversation_lines(&visible, area, buf);
}

fn render_visible_conversation_lines(lines: &[&Line<'static>], area: Rect, buf: &mut Buffer) {
    let mut index = 0;
    let mut y = area.y;
    while index < lines.len() && y < area.bottom() {
        if lines[index]
            .spans
            .first()
            .is_some_and(|span| span.content == DIFF_BLOCK_MARKER)
        {
            let end = lines[index + 1..]
                .iter()
                .position(|line| {
                    line.spans
                        .first()
                        .is_some_and(|span| span.content == DIFF_BLOCK_END_MARKER)
                })
                .map_or(lines.len(), |offset| index + 1 + offset);
            let block_height =
                (end - index + 2).min(area.bottom().saturating_sub(y) as usize) as u16;
            let block_area = Rect::new(area.x, y, area.width, block_height);
            let title = Line::from(lines[index].spans[1..].to_vec());
            let block = Block::default()
                .title(title)
                .borders(Borders::ALL)
                .padding(Padding::horizontal(1))
                .border_style(theme::inactive_panel_border())
                .style(theme::panel());
            let inner = block.inner(block_area);
            block.render(block_area, buf);
            // One row per line, same as an unwrapped `Paragraph` over the same
            // slice, but without cloning the lines to build one.
            for (offset, line) in lines[index + 1..end].iter().enumerate() {
                let row = inner.y.saturating_add(offset as u16);
                if row >= inner.bottom() {
                    break;
                }
                (*line).render(Rect::new(inner.x, row, inner.width, 1), buf);
            }
            y = y.saturating_add(block_height);
            index = end.saturating_add(1);
        } else {
            lines[index].render(Rect::new(area.x, y, area.width, 1), buf);
            y = y.saturating_add(1);
            index += 1;
        }
    }
}

impl Widget for ConversationLinesWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        render_conversation_lines(
            self.lines,
            self.tail_lines,
            self.scroll,
            self.follow,
            self.bottom_padding,
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
        render_conversation_lines(
            &lines,
            &[],
            self.model.scroll,
            self.model.follow,
            0,
            area,
            buf,
        );
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
                outcome: Default::default(),
                role: MessageRole::System,
                content: "You are Forge, a coding agent. Use tools when needed.".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::User,
                content: "hi".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::Assistant,
                content: "yo".into(),
                tool_call_id: None,
                name: None,
                thinking: Some("**ponder**".into()),
                thinking_duration_secs: Some(2.4),
                tool_calls: vec![],
            },
            Message {
                outcome: Default::default(),
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
        // System prompts stay hidden; completed reasoning remains visible before the answer.
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
        let semantic = m.semantic_blocks();
        assert!(
            semantic
                .iter()
                .any(|block| matches!(block, ConversationBlock::ActivityGroup(_))),
            "tool result should classify into semantic activity blocks: {semantic:?}"
        );
        assert!(
            rendered.contains("**ponder** · 2.4s"),
            "completed thought should remain visible, without a spelled-out caption:\n{rendered}"
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
                    subcommand: None,
                    outcome: forge_types::ExecutionOutcome::Success,
                },
                ChatItem::ToolCard {
                    name: "fffind".into(),
                    summary: "needle · 1 file".into(),
                    detail: "src/main.rs".into(),
                    state: ToolCardState::Done,
                    duration: None,
                    subcommand: None,
                    outcome: forge_types::ExecutionOutcome::Success,
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
                    subcommand: None,
                    outcome: forge_types::ExecutionOutcome::Success,
                },
            ],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };

        let blocks = model.semantic_blocks();
        assert!(matches!(
            blocks.first(),
            Some(ConversationBlock::ActivityGroup(_))
        ));
        assert!(matches!(
            blocks.last(),
            Some(ConversationBlock::AssistantAnswer(_))
        ));
    }

    #[test]
    fn completed_turn_composes_activity_before_answer() {
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
                    subcommand: None,
                    outcome: forge_types::ExecutionOutcome::Success,
                },
                ChatItem::ToolCard {
                    name: "fffind".into(),
                    summary: "crate · 3 files".into(),
                    detail: "crates/".into(),
                    state: ToolCardState::Done,
                    duration: None,
                    subcommand: None,
                    outcome: forge_types::ExecutionOutcome::Success,
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
                ConversationBlock::ActivityGroup(_),
                ConversationBlock::AssistantAnswer(a),
            ] if a.text == "Forge is a Rust workspace."
        ));
    }

    #[test]
    fn thinking_is_never_promoted_to_final_answer() {
        let messages = vec![
            Message {
                outcome: Default::default(),
                role: MessageRole::User,
                content: "Summarize this codebase".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                outcome: Default::default(),
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
    fn failed_turn_renders_activity_before_failure() {
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
                    subcommand: None,
                    outcome: forge_types::ExecutionOutcome::Failed { exit_code: None },
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
                ConversationBlock::ActivityGroup(_),
                ConversationBlock::Callout(c),
            ] if matches!(c.kind, BannerKind::Error)
                && c.text.contains("couldn't complete")
        ));
    }

    #[test]
    fn turn_failed_marker_is_not_an_assistant_answer() {
        let messages = vec![
            Message {
                outcome: Default::default(),
                role: MessageRole::User,
                content: "Summarize this codebase".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                outcome: Default::default(),
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
    fn all_assistant_answers_per_turn_are_kept() {
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
        assert_eq!(
            answers,
            vec!["I need to summarize...", "Forge is a Rust workspace."]
        );
    }

    #[test]
    fn assistant_narration_with_tool_calls_survives_streaming() {
        let messages = vec![
            Message {
                outcome: Default::default(),
                role: MessageRole::User,
                content: "inspect the project".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::Assistant,
                content: "I’ll inspect the project first.".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![ToolCall {
                    id: "call-1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "README.md"}),
                }],
            },
        ];
        let model = ConversationModel::from_messages(
            &messages,
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        assert!(model.items.iter().any(|item| matches!(
            item,
            ChatItem::Assistant { text } if text == "I’ll inspect the project first."
        )));
    }

    #[test]
    fn legacy_sessions_render_through_the_adapter_without_migration() {
        let messages = vec![
            Message {
                outcome: Default::default(),
                role: MessageRole::User,
                content: "hello".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                outcome: Default::default(),
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
                subcommand: None,
                outcome: forge_types::ExecutionOutcome::Failed { exit_code: None },
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
    fn completed_thinking_remains_visible_in_lines() {
        let msgs = vec![Message {
            outcome: Default::default(),
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
            text.contains("long thinking"),
            "completed thinking body should remain visible, got:\n{text}"
        );
    }

    #[test]
    fn thinking_renders_dim_italic_indented_and_precedes_the_answer() {
        let msgs = vec![Message {
            outcome: Default::default(),
            role: MessageRole::Assistant,
            content: "final answer".into(),
            tool_call_id: None,
            name: None,
            thinking: Some("reasoning text".into()),
            thinking_duration_secs: Some(2.4),
            tool_calls: vec![],
        }];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            TaskLifecycle::Ready,
            ConversationViewOpts::default(),
        );
        let lines = m.lines_for_width(80);

        let thinking_idx = lines
            .iter()
            .position(|l| line_text(l).contains("reasoning text"))
            .expect("thinking line present");
        let answer_idx = lines
            .iter()
            .position(|l| line_text(l).contains("final answer"))
            .expect("answer line present");
        assert!(
            thinking_idx < answer_idx,
            "thinking must render before the answer, got thinking@{thinking_idx} answer@{answer_idx}: {lines:?}"
        );

        let thinking_line = &lines[thinking_idx];
        let thinking_text = line_text(thinking_line);
        assert!(
            thinking_text.starts_with(&INDENT_UNIT.repeat(2)),
            "thinking should be indented past normal content, got {thinking_text:?}"
        );
        let dark = theme::palette(forge_config::THEME_SOLARIZED_DARK);
        let span = thinking_line
            .spans
            .iter()
            .find(|s| s.content.contains("reasoning text"))
            .expect("thinking span present");
        assert_eq!(
            span.style.fg,
            Some(dark.dim),
            "thinking should use the dim token"
        );
        assert!(
            span.style.add_modifier.contains(Modifier::ITALIC),
            "thinking should be italic"
        );
        assert!(
            !span.style.add_modifier.contains(Modifier::BOLD),
            "thinking should not be bold — no label, unlike tool activity"
        );
        assert!(
            thinking_text.contains("2.4s"),
            "duration should still be shown, got {thinking_text:?}"
        );
    }

    #[test]
    fn wide_viewport_does_not_wrap_at_the_old_column_limit() {
        let content = std::iter::repeat_n("word", 24)
            .collect::<Vec<_>>()
            .join(" ");
        let msgs = vec![Message {
            outcome: Default::default(),
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
        assert_eq!(answer_lines, 2);
    }

    #[test]
    fn active_thinking_is_hidden_from_rendered_lines() {
        let msgs = vec![Message {
            outcome: Default::default(),
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
            outcome: Default::default(),
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
    fn user_messages_render_left_aligned_with_indent() {
        const WIDTH: usize = 100;
        let msgs = vec![Message {
            outcome: Default::default(),
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
        assert_eq!(rendered_lines[0].trim_end(), "  hello world", "{rendered}");
        let dark = theme::palette(forge_config::THEME_SOLARIZED_DARK);
        let first = &lines[0];
        // No leading marker — a plain indent, background carried to the edge.
        assert_eq!(first.spans[0].content.as_ref(), "  ");
        assert_eq!(first.spans[0].style.bg, Some(dark.accent_soft));
        assert_eq!(first.spans[1].content.as_ref(), "hello world");
        assert_eq!(first.spans[1].style.fg, Some(dark.text));
        assert_eq!(first.spans[1].style.bg, Some(dark.accent_soft));
        assert!(!rendered.contains('|'), "{rendered}");
        assert!(!rendered.contains('›'), "{rendered}");
        assert!(!rendered.contains(" │"), "{rendered}");
        assert!(rendered.contains("hello world"), "{rendered}");
    }

    #[test]
    fn wrapped_user_message_keeps_indent_and_background_on_every_row() {
        const WIDTH: usize = 20;
        let msgs = vec![Message {
            outcome: Default::default(),
            role: MessageRole::User,
            content: "one two three four five six seven".into(),
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
        let lines = m.lines_for_width(WIDTH);
        let dark = theme::palette(forge_config::THEME_SOLARIZED_DARK);
        let user_rows: Vec<&Line<'static>> = lines
            .iter()
            .take_while(|line| {
                line.spans
                    .first()
                    .is_some_and(|s| s.style.bg == Some(dark.accent_soft))
            })
            .collect();
        assert!(
            user_rows.len() > 1,
            "message should wrap to more than one row at width {WIDTH}: {lines:?}"
        );
        for row in &user_rows {
            assert_eq!(row.spans[0].content.as_ref(), "  ");
            assert_eq!(row.spans[0].style.bg, Some(dark.accent_soft));
        }
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
            outcome: Default::default(),
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
            outcome: Default::default(),
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
            outcome: Default::default(),
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
            outcome: Default::default(),
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
                    subcommand: None,
                    outcome: forge_types::ExecutionOutcome::Success,
                },
                ChatItem::ToolCard {
                    name: "ffgrep".into(),
                    summary: "needle · 1 match".into(),
                    detail: "a.rs:1:needle".into(),
                    state: ToolCardState::Done,
                    duration: None,
                    subcommand: None,
                    outcome: forge_types::ExecutionOutcome::Success,
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
                    subcommand: None,
                    outcome: forge_types::ExecutionOutcome::Failed { exit_code: None },
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
                if group.count_label.contains("Tests failed")
                    && matches!(group.outcome, ActivityOutcome::Failure)
        )));
    }

    #[test]
    fn failed_validation_command_does_not_render_as_validation_completed() {
        let model = ConversationModel {
            items: vec![ChatItem::ToolCard {
                name: "bash".into(),
                summary: "$ cargo test · failed · exit code 101".into(),
                detail: "status 101".into(),
                state: ToolCardState::Error,
                duration: None,
                subcommand: None,
                outcome: ExecutionOutcome::Failed {
                    exit_code: Some(101),
                },
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let rendered = rendered_text(&model);
        assert!(
            !rendered.contains("Validation completed"),
            "a failed validation command must never render as completed:\n{rendered}"
        );
        assert!(
            rendered.contains("Tests failed") || rendered.contains("failed"),
            "the failure must be visible in the rendered transcript:\n{rendered}"
        );
    }

    #[test]
    fn classify_tool_content_reads_outcome_not_substring_match() {
        // Content that happens to contain the word "validation" must not be
        // classified as an error when the real outcome is Success — the old
        // bug was exactly this: substring-matching rendered text instead of
        // the real execution result.
        let (state, _, _, _) = classify_tool_content(
            "bash",
            "ran the validation suite successfully",
            None,
            &ExecutionOutcome::Success,
        );
        assert_eq!(state, ToolCardState::Done);
    }

    #[test]
    fn classify_tool_content_derives_invocation_from_call_arguments() {
        let call = ToolCall {
            id: "call_1".into(),
            name: "git".into(),
            arguments: serde_json::json!({"subcommand": "status", "args": ["--short"]}),
        };
        let (_, _, invocation, _) =
            classify_tool_content("git", "", Some(&call), &ExecutionOutcome::Success);
        assert_eq!(invocation, Some("git status --short".into()));
    }

    #[test]
    fn classify_tool_content_yields_no_invocation_for_render_only_tools() {
        let call = ToolCall {
            id: "call_2".into(),
            name: "apply_patch".into(),
            arguments: serde_json::json!({"patch": "*** Begin Patch"}),
        };
        let (_, _, invocation, _) = classify_tool_content(
            "apply_patch",
            "...",
            Some(&call),
            &ExecutionOutcome::Success,
        );
        assert_eq!(invocation, None);
    }

    #[test]
    fn classify_tool_content_redacts_sensitive_invocations() {
        let call = ToolCall {
            id: "call_3".into(),
            name: "bash".into(),
            arguments: serde_json::json!({"command": "curl -H 'Authorization: Bearer sk-abc123' /secret"}),
        };
        let (_, _, invocation, _) =
            classify_tool_content("bash", "", Some(&call), &ExecutionOutcome::Success);
        assert_eq!(
            invocation, None,
            "a sensitive command must never surface on the always-visible subcommand line"
        );
    }

    #[test]
    fn classify_tool_content_bash_success_yields_tests_passed_copy() {
        let (state, summary, _, _) =
            classify_tool_content("bash", "", None, &ExecutionOutcome::Success);
        assert_eq!(state, ToolCardState::Done);
        assert!(summary.contains("completed"), "{summary}");
    }

    #[test]
    fn classify_tool_content_bash_nonzero_yields_exit_code_copy() {
        let (state, summary, _, _) = classify_tool_content(
            "bash",
            "boom",
            None,
            &ExecutionOutcome::Failed {
                exit_code: Some(101),
            },
        );
        assert_eq!(state, ToolCardState::Error);
        assert!(summary.contains("failed · exit code 101"), "{summary}");
    }

    #[test]
    fn classify_tool_content_bash_spawn_failed_yields_command_not_found_copy() {
        let (state, summary, _, _) = classify_tool_content(
            "bash",
            "boom",
            None,
            &ExecutionOutcome::SpawnFailed {
                reason: "command not found".into(),
            },
        );
        assert_eq!(state, ToolCardState::Error);
        assert!(summary.contains("failed · command not found"), "{summary}");
    }

    #[test]
    fn classify_tool_content_bash_denied_yields_skipped_denied_copy() {
        let (state, summary, _, _) = classify_tool_content(
            "bash",
            "denied by ACL: bash",
            None,
            &ExecutionOutcome::Denied {
                reason: "denied by ACL: bash".into(),
            },
        );
        assert_eq!(state, ToolCardState::Error);
        assert!(summary.contains("skipped · denied"), "{summary}");
    }

    #[test]
    fn classify_tool_content_bash_cancelled_yields_cancelled_copy() {
        let (state, summary, _, _) =
            classify_tool_content("bash", "", None, &ExecutionOutcome::Cancelled);
        assert_eq!(state, ToolCardState::Error);
        assert!(summary.contains("cancelled"), "{summary}");
    }

    #[test]
    fn classify_tool_content_bash_timed_out_yields_timed_out_copy() {
        let (state, summary, _, _) =
            classify_tool_content("bash", "", None, &ExecutionOutcome::TimedOut);
        assert_eq!(state, ToolCardState::Error);
        assert!(summary.contains("timed out"), "{summary}");
    }

    #[test]
    fn activity_outcome_icon_matrix() {
        let cases = [
            (ActivityOutcome::Success, false),
            (ActivityOutcome::Failure, false),
            (ActivityOutcome::Blocked, false),
            (ActivityOutcome::Warning, false),
            (ActivityOutcome::Neutral, false),
            (ActivityOutcome::Denied, false),
            (ActivityOutcome::Cancelled, false),
            (ActivityOutcome::TimedOut, false),
        ];
        for (outcome, _) in cases {
            let model = ConversationModel {
                items: vec![ChatItem::ActivityGroup {
                    category: ActivityCategory::Validating,
                    summary: "summary".into(),
                    detail: "detail".into(),
                    state: ToolCardState::Done,
                    outcome: match outcome {
                        ActivityOutcome::Denied => ExecutionOutcome::Denied {
                            reason: "denied".into(),
                        },
                        ActivityOutcome::Cancelled => ExecutionOutcome::Cancelled,
                        ActivityOutcome::TimedOut => ExecutionOutcome::TimedOut,
                        ActivityOutcome::Failure => ExecutionOutcome::Failed { exit_code: None },
                        _ => ExecutionOutcome::Success,
                    },
                }],
                scroll: 0,
                follow: true,
                opts: ConversationViewOpts::default(),
            };
            // Rendering must not panic for any outcome variant.
            let _ = rendered_text(&model);
        }
    }

    #[test]
    fn tool_expanded_reveals_successful_tool_details() {
        let model = ConversationModel {
            items: vec![ChatItem::ToolCard {
                name: "read_file".into(),
                summary: "src/lib.rs".into(),
                detail: "full file output".into(),
                state: ToolCardState::Done,
                duration: None,
                subcommand: None,
                outcome: forge_types::ExecutionOutcome::Success,
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts {
                tool_expanded: true,
                ..ConversationViewOpts::default()
            },
        };

        assert!(model.semantic_blocks().iter().any(|block| matches!(
            block,
            ConversationBlock::ActivityGroup(group)
                if group.expanded && group.items.iter().any(|item| item.contains("full file output"))
        )));
    }

    #[test]
    fn inline_code_in_body_text_uses_secondary_body_color_not_interactive_accent() {
        let lines = render_markdown("plain text with `inline code` in it", 80);
        let code_span = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.as_ref() == "inline")
            .expect("inline code token present");
        assert_eq!(code_span.style.fg, Some(theme::text_secondary_color()));
        assert_ne!(code_span.style.fg, Some(theme::accent_color()));
        assert_ne!(code_span.style.fg, Some(theme::info_color()));
    }

    fn bash_tool_card(command: &str, output_lines: &str, expanded: bool) -> ConversationModel {
        ConversationModel {
            items: vec![ChatItem::ToolCard {
                name: "bash".into(),
                summary: format!("$ {command} · {} output lines", output_lines),
                detail: (0..output_lines.parse::<usize>().unwrap_or(0))
                    .map(|i| format!("line {i}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                state: ToolCardState::Done,
                duration: None,
                subcommand: Some(format!("$ {command}")),
                outcome: forge_types::ExecutionOutcome::Success,
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts {
                tool_expanded: expanded,
                ..ConversationViewOpts::default()
            },
        }
    }

    #[test]
    fn long_command_subcommand_wraps_with_connector() {
        let long_command = "cargo build --workspace --all-features --jobs 8 && cargo doc --no-deps; ls -la; git status --short";
        let model = bash_tool_card(long_command, "5", false);
        let text = rendered_text(&model);

        assert!(text.contains("└─ $ cargo build"), "{text}");
        // A command too wide for the pane wraps with the connector carried
        // down, so the full invocation stays visible instead of truncating.
        assert!(text.contains("--short"), "{text}");
        assert!(
            text.contains("│"),
            "wrapped continuation must carry the connector:\n{text}"
        );
        assert!(text.contains("5 output lines"), "{text}");
        assert!(text.contains("[Ctrl + o]"), "{text}");
    }

    #[test]
    fn long_command_entry_expands_full_text_via_ctrl_o() {
        let long_command = "cargo test --workspace --all-features -- --test-threads=1 --nocapture; git diff --check; git status --short";
        let model = bash_tool_card(long_command, "5", true);
        let text = rendered_text(&model);

        // Wrapping the expanded detail can reflow incidental whitespace, so
        // check for the command's distinct pieces rather than byte-exact
        // equality with the original string.
        assert!(text.contains("--test-threads=1 --nocapture"), "{text}");
        assert!(text.contains("git status --short"), "{text}");
        assert!(text.contains("[Ctrl + o] collapse"), "{text}");
    }

    #[test]
    fn short_command_entries_are_unaffected() {
        let short_command = "cargo test -p forge-tui";
        let model = bash_tool_card(short_command, "3", false);
        let text = rendered_text(&model);

        assert!(text.contains(&format!("$ {short_command}")), "{text}");
        assert!(!text.contains('…'), "{text}");
    }

    #[test]
    fn tool_card_with_subcommand_renders_connector_line() {
        let model = ConversationModel {
            items: vec![ChatItem::ToolCard {
                name: "git".into(),
                summary: "git status --short · 12 output lines".into(),
                detail: " M crates/forge-tui/src/conversation.rs".into(),
                state: ToolCardState::Done,
                duration: None,
                subcommand: Some("git status --short".into()),
                outcome: forge_types::ExecutionOutcome::Success,
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let text = rendered_text(&model);

        assert!(
            text.contains("└─ git status --short · 12 output lines"),
            "invocation must render on its own connector line:\n{text}"
        );
        assert!(
            !text.contains("1 item"),
            "count label must move off the label line:\n{text}"
        );
    }

    #[test]
    fn tool_card_without_subcommand_keeps_single_line() {
        // Render-only tools (apply_patch, MCP) keep the old single-line form.
        let model = ConversationModel {
            items: vec![ChatItem::ToolCard {
                name: "apply_patch".into(),
                summary: "patch applies cleanly".into(),
                detail: "*** Begin Patch".into(),
                state: ToolCardState::Done,
                duration: None,
                subcommand: None,
                outcome: forge_types::ExecutionOutcome::Success,
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let text = rendered_text(&model);
        assert!(!text.contains('└'), "{text}");
        assert!(text.contains("Implemented changes"), "{text}");
        assert!(
            !text.contains("patch applies cleanly"),
            "summary stays expanded-only:\n{text}"
        );
    }

    #[test]
    fn write_tool_subcommand_uses_path_when_summary_is_output_preview() {
        let model = ConversationModel {
            items: vec![ChatItem::ToolCard {
                name: "write_file".into(),
                summary: "wrote · +1 -0 src/foo.rs".into(),
                detail: "wrote src/foo.rs".into(),
                state: ToolCardState::Done,
                duration: None,
                subcommand: Some("src/foo.rs".into()),
                outcome: forge_types::ExecutionOutcome::Success,
            }],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let text = rendered_text(&model);
        assert!(
            text.contains("└─ src/foo.rs"),
            "write tools should surface the path, not the output preview:\n{text}"
        );
    }

    #[test]
    fn collapsed_command_summary_splits_at_first_pipe_or_semicolon() {
        let items = vec!["a\nb\nc".to_string()];
        let (command, output_lines) = collapsed_command_summary(
            "$ cargo test --workspace; git diff --check; git status --short --pad-past-eighty-chars-total-length-of-this-line",
            &items,
        )
        .expect("long command should collapse");
        assert_eq!(command, "$ cargo test --workspace");
        assert_eq!(output_lines, 3);
    }

    #[test]
    fn collapsed_command_summary_ellipsizes_an_overlong_single_segment() {
        let items: Vec<String> = vec![];
        let long_single_segment = "cargo test --workspace --all-features --lib --bins --tests --examples --benches --no-fail-fast";
        let (command, _) =
            collapsed_command_summary(&format!("$ {long_single_segment}"), &items).expect("long");
        assert!(command.ends_with('…'), "{command}");
        assert!(command.chars().count() <= COMMAND_LINE_MAX_CHARS + "$ …".chars().count());
    }

    #[test]
    fn collapsed_command_summary_ignores_short_commands() {
        let items: Vec<String> = vec![];
        assert_eq!(
            collapsed_command_summary("$ cargo test -p forge-tui", &items),
            None
        );
        // Non-command summaries (file counts, etc.) are never affected.
        assert_eq!(collapsed_command_summary("3 files inspected", &items), None);
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
                    subcommand: None,
                    outcome: forge_types::ExecutionOutcome::Success,
                },
                ChatItem::ToolCard {
                    name: "apply_patch".into(),
                    summary: "wrote · src/app.rs".into(),
                    detail: "src/app.rs".into(),
                    state: ToolCardState::Done,
                    duration: None,
                    subcommand: None,
                    outcome: forge_types::ExecutionOutcome::Success,
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
        assert_eq!(activity_detail_label(true), "  [Ctrl + o] collapse");
        assert_eq!(activity_detail_label(false), "  [Ctrl + o]");
    }

    #[test]
    fn validation_failure_is_deduplicated_and_labels_retry() {
        let error = "Tool validation error: tool `read_file` validation failed at /path: 1 is not of type string. Please correct arguments.";
        let msgs = vec![
            Message {
                outcome: Default::default(),
                role: MessageRole::Tool,
                content: error.into(),
                tool_call_id: Some("1".into()),
                name: Some("read_file".into()),
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::Tool,
                content: error.into(),
                tool_call_id: Some("2".into()),
                name: Some("read_file".into()),
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                outcome: Default::default(),
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
            outcome: Default::default(),
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
    fn update_plan_tool_messages_render_as_checklist() {
        let msgs = vec![
            Message {
                outcome: Default::default(),
                role: MessageRole::Assistant,
                content: String::new(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![ToolCall {
                    id: "plan-1".into(),
                    name: "update_plan".into(),
                    arguments: serde_json::json!({
                        "explanation": "Next steps",
                        "plan": [
                            {"step": "Inspect code", "status": "completed"},
                            {"step": "Implement tool", "status": "in_progress"},
                            {"step": "Add tests", "status": "pending"}
                        ]
                    }),
                }],
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::Tool,
                content: "Plan updated".into(),
                tool_call_id: Some("plan-1".into()),
                name: Some("update_plan".into()),
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
        ];
        let m = ConversationModel::from_messages(
            &msgs,
            &[],
            TaskLifecycle::Ready,
            ConversationViewOpts::default(),
        );
        assert!(
            matches!(
                &m.items[..],
                [ChatItem::PlanChecklist {
                    explanation: Some(exp),
                    steps
                }] if exp == "Next steps" && steps.len() == 3
            ),
            "expected plan checklist item, got {:?}",
            m.items
        );
        let text = m
            .lines_for_width(80)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        // No spelled-out "Plan" caption by design — the bordered card and
        // [✓]/[►]/[ ] checkboxes carry the meaning instead.
        assert!(text.contains("Next steps"), "{text}");
        assert!(text.contains("Inspect code"), "{text}");
        assert!(text.contains("Implement tool"), "{text}");
        assert!(text.contains("Add tests"), "{text}");
        assert!(matches!(
            m.semantic_blocks().as_slice(),
            [ConversationBlock::PlanChecklist(_)]
        ));
    }

    #[test]
    fn plan_checklist_card_is_bordered_with_no_background_fill() {
        let plan = PlanChecklistPresentation {
            explanation: Some("Next steps".into()),
            steps: vec![forge_types::PlanItem {
                step: "Inspect code".into(),
                status: forge_types::PlanStepStatus::Completed,
            }],
        };
        let lines = render_plan_checklist(&plan, 80);
        // The explanation ("Next steps") renders as an unboxed intro line
        // above the card — only the step list itself is bordered.
        let top = lines
            .iter()
            .find(|l| line_text(l).starts_with('┌'))
            .expect("top border present");
        assert!(
            !line_text(top).contains("Plan"),
            "no spelled-out caption in the border, got {:?}",
            line_text(top)
        );
        assert!(
            lines
                .iter()
                .rev()
                .find(|l| !line_text(l).is_empty())
                .is_some_and(|l| line_text(l).starts_with('└')),
            "plan card should close with a bottom border"
        );
        let content_row = lines
            .iter()
            .find(|l| line_text(l).contains("Inspect code"))
            .expect("step content row present");
        for span in &content_row.spans {
            assert_eq!(
                span.style.bg, None,
                "plan card content must have no background fill — canvas shows through, got {span:?}"
            );
        }
    }

    #[test]
    fn multi_file_diff_results_become_separate_cards() {
        let msgs = vec![Message {
            outcome: Default::default(),
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

    #[test]
    fn diff_pane_borders_multi_hunk_content_without_overflowing() {
        let diff = [
            "@@ -1 +1 @@",
            "-old",
            "+new",
            "@@ -10 +10 @@",
            "-before",
            "+after",
            "@@ -20 +20 @@",
            "+this line is deliberately longer than one hundred characters so a narrow diff pane clips it instead of breaking its layout",
        ]
        .map(str::to_string);
        let lines = vec![diff_title_line("src/lib.rs", &diff)]
            .into_iter()
            .chain(render_numbered_diff("src/lib.rs", &diff, 38))
            .chain(std::iter::once(Line::from(DIFF_BLOCK_END_MARKER)))
            .collect::<Vec<_>>();
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);

        let lines = lines.iter().collect::<Vec<_>>();
        render_visible_conversation_lines(&lines, area, &mut buf);

        assert_eq!(buf[(0, 0)].symbol(), "┌");
        assert_eq!(buf[(39, 0)].symbol(), "┐");
        assert!(buf[(2, 0)].symbol().contains("s"));
        assert_eq!(buf[(0, 10)].symbol(), "└");
        assert_eq!(buf[(39, 10)].symbol(), "┘");
    }

    #[test]
    fn diff_hunk_headers_align_with_file_header() {
        let diff = ["@@ -1 +1 @@", "-old", "+new"].map(str::to_string);
        let title = lines_text(&[diff_title_line("src/lib.rs", &diff)]);
        let hunk = lines_text(&render_numbered_diff("src/lib.rs", &diff, 40));

        assert!(title.starts_with("\u{200b} src/lib.rs"));
        assert!(hunk.starts_with("@@ -1 +1 @@"));
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

        let text = lines_text(&render_markdown(streaming, 80));

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

        let open_lines = render_markdown(open, 80);
        let closed_lines = render_markdown(closed, 80);

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
        let text = lines_text(&render_markdown("```rust\nlet x = 1;", 80));

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
    fn pending_approval_renders_full_redacted_payload_inline() {
        let m = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Waiting,
            ConversationViewOpts::default(),
        )
        .with_pending_approval(
            ApprovalRequestView {
                tool: "bash".into(),
                command: "git push -u origin feature".into(),
                cwd: "workspace".into(),
                env_delta: "inherited".into(),
            },
            vec![
                ApprovalMenuRow {
                    label: "Allow once".into(),
                    detail: None,
                },
                ApprovalMenuRow {
                    label: "Allow pattern going forward".into(),
                    detail: Some("bash(git push *)".into()),
                },
                ApprovalMenuRow {
                    label: "Deny".into(),
                    detail: None,
                },
            ],
            0,
            false,
        );
        let text = m
            .lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("⏸ APPROVAL REQUIRED"), "{text}");
        assert!(text.contains("git push -u origin feature"), "{text}");
        assert!(text.contains("cwd: workspace"), "{text}");
        assert!(text.contains("env: inherited"), "{text}");
        assert!(text.contains("› Allow once"), "{text}");
        assert!(text.contains("bash(git push *)"), "{text}");
        assert!(
            text.contains("↑↓ select · Enter confirm · Esc cancel"),
            "{text}"
        );
    }

    #[test]
    fn approval_card_hugs_short_content_instead_of_spanning_the_full_pane() {
        const PANE_WIDTH: usize = 100;
        let m = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Waiting,
            ConversationViewOpts::default(),
        )
        .with_pending_approval(
            ApprovalRequestView {
                tool: "bash".into(),
                command: "ls".into(),
                cwd: "wd".into(),
                env_delta: "inherited".into(),
            },
            vec![ApprovalMenuRow {
                label: "Allow once".into(),
                detail: None,
            }],
            0,
            false,
        );
        let lines = m.lines_for_width(PANE_WIDTH);
        let top_border = lines
            .iter()
            .find(|l| line_text(l).starts_with('┌'))
            .expect("top border present");
        let border_width = line_text(top_border).chars().count();
        assert!(
            border_width < PANE_WIDTH,
            "a short command's card should not span the full {PANE_WIDTH}-col pane, got {border_width}: {lines:?}"
        );
        // But every content row and the bottom border must still match the
        // top border's width exactly, or the box wouldn't line up.
        let bottom_border = lines
            .iter()
            .rev()
            .find(|l| line_text(l).starts_with('└'))
            .expect("bottom border present");
        assert_eq!(line_text(bottom_border).chars().count(), border_width);
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
                outcome: Default::default(),
                role: MessageRole::User,
                content: "[REPAIR TASK EVAL-01]\nSENSOR · DETERMINISTIC\ncargo test · failed\nEVALUATOR REPORT\nCriteria: public API returns 429\nFinding: layer is registered too late\nRepair: attach layer to public router".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                outcome: Default::default(),
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

    fn three_block_model() -> ConversationModel {
        let msgs = vec![
            Message {
                outcome: Default::default(),
                role: MessageRole::User,
                content: "first".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::Assistant,
                content: "second".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::User,
                content: "third".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
        ];
        ConversationModel::from_messages(
            &msgs,
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        )
    }

    fn is_rule_line(line: &Line<'static>) -> bool {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        !text.is_empty() && text.chars().all(|c| c == '─')
    }

    fn rule_lines<'a>(lines: &'a [Line<'static>]) -> Vec<&'a Line<'static>> {
        lines.iter().filter(|line| is_rule_line(line)).collect()
    }

    fn rendered_text(model: &ConversationModel) -> String {
        model
            .lines()
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    fn line_text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn tool_turn_items() -> Vec<ChatItem> {
        vec![
            ChatItem::User {
                text: "fix the failing test".into(),
            },
            ChatItem::ToolCard {
                name: "read_file".into(),
                summary: "src/lib.rs · 2 lines".into(),
                detail: "src/lib.rs".into(),
                state: ToolCardState::Done,
                duration: None,
                subcommand: None,
                outcome: forge_types::ExecutionOutcome::Success,
            },
            ChatItem::ToolCard {
                name: "run_shell".into(),
                summary: "cargo test · failed".into(),
                detail: "output".into(),
                state: ToolCardState::Done,
                duration: None,
                subcommand: Some("cargo test".into()),
                outcome: forge_types::ExecutionOutcome::Failed {
                    exit_code: Some(101),
                },
            },
            ChatItem::Assistant {
                text: "Root cause: float rounding.".into(),
            },
        ]
    }

    fn tool_turn_model() -> ConversationModel {
        ConversationModel {
            items: tool_turn_items(),
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        }
    }

    fn plan_items() -> Vec<forge_types::PlanItem> {
        use forge_types::PlanStepStatus;
        vec![
            forge_types::PlanItem {
                step: "Inspect failure".into(),
                status: PlanStepStatus::Completed,
            },
            forge_types::PlanItem {
                step: "Fix float comparison".into(),
                status: PlanStepStatus::InProgress,
            },
        ]
    }

    fn planned_turn_model() -> ConversationModel {
        ConversationModel {
            items: vec![
                ChatItem::User {
                    text: "fix the failing test".into(),
                },
                ChatItem::PlanChecklist {
                    explanation: Some("Next steps".into()),
                    steps: plan_items(),
                },
                // A later plan_update in the same turn re-renders the
                // checklist but must not open another phase rule.
                ChatItem::PlanChecklist {
                    explanation: Some("Next steps".into()),
                    steps: plan_items(),
                },
                ChatItem::ToolCard {
                    name: "run_shell".into(),
                    summary: "cargo test · passed".into(),
                    detail: "output".into(),
                    state: ToolCardState::Done,
                    duration: None,
                    subcommand: Some("cargo test".into()),
                    outcome: forge_types::ExecutionOutcome::Success,
                },
                ChatItem::Assistant {
                    text: "Fixed.".into(),
                },
            ],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        }
    }

    #[test]
    fn every_turn_boundary_gets_a_rule_even_without_a_plan() {
        let model = three_block_model();
        assert_eq!(model.semantic_blocks().len(), 3);

        let lines = model.lines_for_width(60);
        // [question, answer, question]: two turns, no plan checklist at all.
        // The rule is a turn-boundary marker now, not tied to plan presence,
        // so the second turn still opens with exactly one rule.
        assert_eq!(
            rule_lines(&lines).len(),
            1,
            "a new turn opens with a rule even without a plan"
        );

        let first = lines.first().expect("non-empty transcript");
        assert!(!is_rule_line(first), "no separator before the first entry");

        let rule_pos = lines.iter().position(is_rule_line).expect("rule present");
        let after = lines[rule_pos + 1..]
            .iter()
            .map(line_text)
            .find(|t| !t.is_empty())
            .expect("content after the rule");
        assert!(
            after.contains("third"),
            "the rule immediately precedes the second turn's user message, got {after:?}"
        );
    }

    #[test]
    fn plan_checklists_alone_do_not_open_a_rule_only_turn_boundaries_do() {
        // `planned_turn_model()` is a single turn containing two consecutive
        // PlanChecklist items (a plan_update). The rule is now purely a
        // turn-boundary marker, decoupled from plan presence — so a
        // single-turn conversation gets zero rules, plan or no plan, and a
        // second plan_update in the same turn still doesn't add one either.
        let model = planned_turn_model();
        let lines = model.lines_for_width(80);
        assert_eq!(
            rule_lines(&lines).len(),
            0,
            "a plan checklist alone must not open a rule without a new turn"
        );
    }

    #[test]
    fn hairline_rule_width_tracks_pane_width() {
        let model = three_block_model();
        for width in [40usize, 90usize] {
            let lines = model.lines_for_width(width);
            let rules = rule_lines(&lines);
            assert_eq!(rules.len(), 1);
            for rule in rules {
                let text = line_text(rule);
                assert_eq!(text.chars().count(), width.max(4));
            }
        }
    }

    #[test]
    fn tool_activity_groups_on_the_turn_rail() {
        let model = tool_turn_model();
        let lines = model.lines_for_width(80);
        assert_eq!(rule_lines(&lines).len(), 0);

        let railed: Vec<&Line<'static>> = lines
            .iter()
            .filter(|l| line_text(l).starts_with('│'))
            .collect();
        assert!(!railed.is_empty(), "tool trail renders on the rail");
        assert!(railed
            .iter()
            .any(|l| line_text(l).contains("Explored repository")));
        assert!(railed.iter().any(|l| line_text(l).contains("cargo test")));

        // User message and final answer break out of the rail.
        let user = lines
            .iter()
            .find(|l| line_text(l).contains("fix the failing test"))
            .expect("user message");
        assert!(!line_text(user).starts_with('│'));
        let answer = lines
            .iter()
            .find(|l| line_text(l).contains("Root cause"))
            .expect("answer");
        assert!(!line_text(answer).starts_with('│'));
        let answer_idx = lines
            .iter()
            .position(|l| line_text(l).contains("Root cause"))
            .unwrap();
        assert!(
            lines[answer_idx - 1]
                .spans
                .iter()
                .all(|s| s.content.is_empty()),
            "the answer is separated from the rail trail by a blank line"
        );
    }

    #[test]
    fn narrow_panes_drop_the_rail() {
        let model = tool_turn_model();
        let lines = model.lines_for_width(40);
        assert!(lines.iter().all(|l| !line_text(l).contains('│')));
        assert_eq!(rule_lines(&lines).len(), 0);
    }

    #[test]
    fn expand_does_not_change_rule_or_rail_structure() {
        let collapsed = ConversationModel {
            items: tool_turn_items(),
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let expanded = ConversationModel {
            items: tool_turn_items(),
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts {
                tool_expanded: true,
                ..Default::default()
            },
        };
        let collapsed_lines = collapsed.lines_for_width(80);
        let expanded_lines = expanded.lines_for_width(80);
        assert_eq!(
            rule_lines(&collapsed_lines).len(),
            rule_lines(&expanded_lines).len(),
            "expand must not change phase boundaries"
        );
        let railed =
            |ls: &[Line<'static>]| ls.iter().filter(|l| line_text(l).starts_with('│')).count();
        assert!(
            railed(&expanded_lines) >= railed(&collapsed_lines),
            "expanded tool items stay on the rail"
        );
    }

    #[test]
    fn rule_and_rail_structure_is_identical_across_themes() {
        let model = planned_turn_model();
        let registry = crate::theme_registry::ThemeRegistry::load(None);
        let mut baseline: Option<(usize, usize)> = None;
        for id in [
            "gruvbox-dark",
            "kanagawa-wave",
            "catppuccin-mocha",
            "solarized-dark",
            "solarized-light",
        ] {
            crate::theme::install(registry.clone(), id);
            let lines = model.lines_for_width(80);
            let cur = (
                rule_lines(&lines).len(),
                lines
                    .iter()
                    .filter(|l| line_text(l).starts_with('│'))
                    .count(),
            );
            if let Some(prev) = baseline {
                assert_eq!(cur, prev, "structural layout differs under theme {id}");
            } else {
                baseline = Some(cur);
            }
        }
    }
}
