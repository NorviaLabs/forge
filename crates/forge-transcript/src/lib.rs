//! Projecting a session into a conversation transcript.
//!
//! What the transcript *shows* — messages and events reduced to
//! [`ChatItem`]s, grouped into [`ConversationBlock`]s, and shaped into the
//! `*Presentation` types. How it is *drawn* is not here and cannot be: this
//! crate has no terminal dependency, which is what lets a headless caller
//! project a transcript without linking a UI.

use forge_core::{AgentSession, TurnEvent, TURN_FAILED_MARKER};
use forge_types::{
    is_readonly_git_subcommand, ExecutionOutcome, Message, MessageRole, TaskLifecycle, ToolCall,
};

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
        /// Model label, provider label and whether the provider is live. The
        /// first screen used to name none of them.
        model: String,
        provider: String,
        connected: bool,
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
        /// no real process/failure concept (e.g. `read_file`, `glob`).
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
    /// Pending `ask_user_question` questionnaire, rendered inline like
    /// approval (not a boxed card).
    QuestionPending(QuestionPendingPresentation),
    /// Structured TODO checklist from the `update_plan` tool.
    PlanChecklist {
        explanation: Option<String>,
        steps: Vec<forge_types::PlanItem>,
    },
    Banner {
        text: String,
        kind: BannerKind,
    },
    /// Closing line of a finished turn: how long it took and what it cost.
    ///
    /// Without one, a turn had no visible end — the answer simply stopped and
    /// only the footer recorded that anything had concluded.
    TurnSummary {
        secs: f64,
        /// Characters of answer text streamed. Characters, not tokens: no
        /// provider reports token usage mid-stream, and an estimate dressed
        /// up as a count is worse than an exact number of something else.
        chars: usize,
        tools: usize,
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
    Home(HomePresentation),
    QuestionPending(QuestionPendingPresentation),
    PlanChecklist(PlanChecklistPresentation),
    Metadata(MetadataPresentation),
    Thinking(ThinkingPresentation),
    TurnSummary(TurnSummaryPresentation),
}

/// The closing line of a finished turn.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnSummaryPresentation {
    pub secs: f64,
    pub chars: usize,
    pub tools: usize,
}

/// Model reasoning. Recedes rather than announces: dim italic, indented past
/// tool activity, no glyph or status word — distinct from `ActiveProgress`
/// (which still owns in-flight tool-call status).
#[derive(Debug, Clone, PartialEq)]
pub struct ThinkingPresentation {
    pub text: String,
    pub duration_secs: Option<f64>,
    /// Spent reasoning: every step of it used to stay on screen in full, so a
    /// multi-tool turn strewed dim orphan paragraphs between its tool rows.
    /// All but the newest collapse to a single line; `tool_expanded` (Ctrl+O)
    /// brings them back.
    pub collapsed: bool,
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
    pub question: Option<String>,
    /// Why this call was gated, in the operator's words. Shown under the
    /// question so the prompt does not read as arbitrary.
    pub reason: Option<String>,
    /// What the sandbox actually reported for *this* command. `reason`
    /// explains the category and reads the same for every command in it;
    /// this is the evidence, and it leads the card when present.
    pub failure: Option<String>,
}

/// The first screen: who you are talking to, where, and a way in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomePresentation {
    pub workspace: String,
    pub skills_loaded: usize,
    pub model: String,
    pub provider: String,
    pub connected: bool,
}

/// One selectable row on the inline approval menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalMenuRow {
    pub label: String,
    pub detail: Option<String>,
    /// Consequence shown only while this row is selected.
    pub help: Option<String>,
    /// Single-key shortcut that picks this row, shown beside its label.
    pub key: Option<String>,
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
    pub question: Option<String>,
    /// Why this call was gated. See [`ApprovalRequestView::reason`].
    pub reason: Option<String>,
    /// See [`ApprovalRequestView::failure`].
    pub failure: Option<String>,
    pub options: Vec<ApprovalMenuRow>,
    pub selected: usize,
    /// Whether the approval card itself holds focus (accent border vs muted).
    pub focused: bool,
}

/// One option row on the inline question prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionMenuRow {
    pub label: String,
    pub description: Option<String>,
    pub chosen: bool,
}

/// The questionnaire awaiting a human answer, shown one question at a time.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestionPendingPresentation {
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionMenuRow>,
    pub selected: usize,
    pub multi_select: bool,
    pub question_index: usize,
    pub question_count: usize,
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
        _events: &[TurnEvent],
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
                            items.push(ChatItem::Thinking {
                                text: th.clone(),
                                duration_secs: m.thinking_duration_secs,
                            });
                        }
                    }
                    // Terminal failure summaries are durable state for resume and header
                    // status, not transcript content.
                    if m.content.starts_with(TURN_FAILED_MARKER) {
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
        items = group_routine_activity(items);
        if status == TaskLifecycle::Waiting {
            items.push(ChatItem::Banner {
                text: "Waiting · approval".into(),
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

    /// Streaming assistant preview. Live reasoning is shown above the answer
    /// so the turn reads thought-then-reply, including while tokens arrive.
    pub fn with_streaming_preview(
        mut self,
        thinking: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        let thinking = thinking.into();
        let text = text.into();
        if !thinking.trim().is_empty()
            && !self.items.iter().any(|item| {
                matches!(
                    item,
                    ChatItem::Thinking { text, .. } if text == &thinking
                )
            })
        {
            self.items.push(ChatItem::Thinking {
                text: thinking,
                duration_secs: self.opts.stream_thought_secs,
            });
        }
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

    pub fn with_home(
        mut self,
        workspace: String,
        skills_loaded: usize,
        model: String,
        provider: String,
        connected: bool,
    ) -> Self {
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
                    model,
                    provider,
                    connected,
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
                question: request.question,
                reason: request.reason,
                failure: request.failure,
                options,
                selected,
                focused,
            }));
        self
    }

    /// Append the pending questionnaire as a full inline transcript item.
    pub fn with_pending_question(mut self, presentation: QuestionPendingPresentation) -> Self {
        self.items.push(ChatItem::QuestionPending(presentation));
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
                    collapsed: false,
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
                                // `Done` means the tool finished, not that it
                                // succeeded: a command that exits non-zero
                                // completes normally. Reading `Done` as success
                                // drew a green check over `curl: (56) CONNECT
                                // tunnel failed`.
                                ToolCardState::Done => ActivityOutcome::from(outcome),
                                ToolCardState::Blocked => ActivityOutcome::Blocked,
                                ToolCardState::Error => ActivityOutcome::from(outcome),
                            },
                            expanded: tool_expanded,
                            subcommands: subcommand_line(subcommand.as_deref(), summary),
                            // Just the detail. The collapsed row already shows
                            // the tool name, and `subcommands` already shows
                            // the summary, so repeating `name: summary` here
                            // printed the same line twice under itself.
                            items: vec![detail.clone()],
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
                    ToolCardState::Done => ActivityOutcome::from(outcome),
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
            ChatItem::QuestionPending(presentation) => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::QuestionPending(presentation.clone()));
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
                model,
                provider,
                connected,
            } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::Home(HomePresentation {
                    workspace: workspace.clone(),
                    skills_loaded: *skills_loaded,
                    model: model.clone(),
                    provider: provider.clone(),
                    connected: *connected,
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
            ChatItem::TurnSummary { secs, chars, tools } => {
                flush_progress(&mut blocks, &mut progress);
                flush_activity(&mut blocks, &mut activity_group);
                blocks.push(ConversationBlock::TurnSummary(TurnSummaryPresentation {
                    secs: *secs,
                    chars: *chars,
                    tools: *tools,
                }));
            }
        }
    }
    flush_progress(&mut blocks, &mut progress);
    flush_activity(&mut blocks, &mut activity_group);
    let blocks = if tool_expanded {
        blocks
    } else {
        collapse_spent_reasoning(blocks)
    };
    finalize_presentation(blocks)
}

/// Keep item order. Collapse adjacent streaming snapshots and duplicate
/// terminal-failure banners; do not re-bucket activity above answers.
fn finalize_presentation(blocks: Vec<ConversationBlock>) -> Vec<ConversationBlock> {
    collapse_duplicate_turn_failures(collapse_adjacent_streaming_snapshots(blocks))
}

/// Fold every reasoning block but the newest down to one line.
///
/// Reasoning was permanent: a turn that ran three tools left three dim
/// paragraphs strewn between its tool rows, none of which the reader still
/// needed. The newest stays open because it is the one describing what is
/// happening now; `tool_expanded` (Ctrl+O) restores the rest.
fn collapse_spent_reasoning(mut blocks: Vec<ConversationBlock>) -> Vec<ConversationBlock> {
    let last = blocks
        .iter()
        .rposition(|block| matches!(block, ConversationBlock::Thinking(_)));
    let Some(last) = last else {
        return blocks;
    };
    for (index, block) in blocks.iter_mut().enumerate() {
        if let ConversationBlock::Thinking(thinking) = block {
            thinking.collapsed = index != last;
        }
    }
    blocks
}

fn is_streaming_answer(block: &ConversationBlock) -> bool {
    matches!(
        block,
        ConversationBlock::AssistantAnswer(answer) if answer.streaming
    )
}

fn is_error_callout(block: &ConversationBlock) -> bool {
    matches!(
        block,
        ConversationBlock::Callout(callout) if matches!(callout.kind, BannerKind::Error)
    )
}

/// Consecutive streaming previews are snapshots of one answer; keep the newest.
/// Durable assistant messages stay visible, including around tool activity.
fn collapse_adjacent_streaming_snapshots(blocks: Vec<ConversationBlock>) -> Vec<ConversationBlock> {
    let mut out = Vec::with_capacity(blocks.len());
    for block in blocks {
        if is_streaming_answer(&block) && out.last().is_some_and(is_streaming_answer) {
            out.pop();
        }
        out.push(block);
    }
    out
}

/// One terminal failure summary per user turn, left in event order.
fn collapse_duplicate_turn_failures(blocks: Vec<ConversationBlock>) -> Vec<ConversationBlock> {
    let mut out = Vec::with_capacity(blocks.len());
    let mut segment = Vec::new();

    let flush_segment = |out: &mut Vec<ConversationBlock>, segment: &mut Vec<ConversationBlock>| {
        let error_count = segment
            .iter()
            .filter(|block| is_error_callout(block))
            .count();
        let mut seen = 0;
        for block in segment.drain(..) {
            if is_error_callout(&block) {
                seen += 1;
                if seen < error_count {
                    continue;
                }
            }
            out.push(block);
        }
    };

    for block in blocks {
        if matches!(block, ConversationBlock::UserMessage(_)) {
            flush_segment(&mut out, &mut segment);
        }
        segment.push(block);
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

/// Collapse consecutive routine tool activity into `ChatItem::ActivityGroup`
/// entries, so a long run of reads and edits reads as one line rather than
/// one line each. Non-routine items pass through untouched.
pub fn group_routine_activity(items: Vec<ChatItem>) -> Vec<ChatItem> {
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
    // No single-item shortcut: a lone routine card used to render full-height
    // and then *collapse* when a sibling completed, shrinking the block under
    // everything above it. Grouping from the first card means arrival only ever
    // adds height. `routine_group_height_never_shrinks` holds this.
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
        "read_file" | "ls" | "glob" | "grep" | "rg" => Some(ActivityCategory::Exploring),
        "git"
            if tool_argument(call, "subcommand").is_some_and(is_readonly_git_subcommand)
                || summary
                    .split_whitespace()
                    .nth(1)
                    .is_some_and(is_readonly_git_subcommand) =>
        {
            Some(ActivityCategory::Exploring)
        }
        "write_file" | "apply_patch" | "edit" | "search_replace" | "edit_file" => {
            Some(ActivityCategory::Implementing)
        }
        "bash" if is_validation_command(summary.trim_start_matches("$ ")) => {
            Some(ActivityCategory::Validating)
        }
        _ => None,
    }
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
                .filter(|item| matches!(item, ChatItem::ToolCard { name, .. } if name == "glob" || name == "grep" || name == "rg"))
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
        "view_image" => {
            let path = tool_argument(call, "path").unwrap_or("image");
            if !outcome.is_success() {
                format!("{path} · failed")
            } else if detail.contains("no longer available") {
                format!("{path} · missing")
            } else {
                let size = detail
                    .split(" · ")
                    .nth(1)
                    .unwrap_or("loaded")
                    .trim()
                    .to_string();
                format!("{path} · {size}")
            }
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
        "glob" => {
            let label = if lower.contains("no files found") {
                "no matches".to_string()
            } else {
                result_count_label(count, "file", "files")
            };
            tool_argument(call, "pattern")
                .map(|query| format!("{query} · {label}"))
                .unwrap_or(label)
        }
        "grep" | "rg" => {
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

pub fn wrap(s: &str, width: usize) -> Vec<String> {
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
mod tests {
    use super::*;
    use forge_types::Message;

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
                    name: "glob".into(),
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
    fn semantic_blocks_preserve_assistant_then_activity_order() {
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
            Some(ConversationBlock::AssistantAnswer(_))
        ));
        assert!(matches!(
            blocks.last(),
            Some(ConversationBlock::ActivityGroup(_))
        ));
    }

    #[test]
    fn completed_turn_keeps_item_order_of_activity_then_answer() {
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
                    name: "glob".into(),
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
    fn semantic_blocks_preserve_interleaved_narration_and_tools() {
        let model = ConversationModel {
            items: vec![
                ChatItem::User {
                    text: "fix the parser".into(),
                },
                ChatItem::Assistant {
                    text: "I’ll edit foo.rs".into(),
                },
                ChatItem::ToolCard {
                    name: "read_file".into(),
                    summary: "src/foo.rs · 1 line".into(),
                    detail: "src/foo.rs".into(),
                    state: ToolCardState::Done,
                    duration: None,
                    subcommand: None,
                    outcome: forge_types::ExecutionOutcome::Success,
                },
                ChatItem::Assistant {
                    text: "now a test".into(),
                },
                ChatItem::DiffCard {
                    path: "src/foo.rs".into(),
                    lines: vec![
                        "diff --git a/src/foo.rs b/src/foo.rs".into(),
                        "--- a/src/foo.rs".into(),
                        "+++ b/src/foo.rs".into(),
                        "@@ -1 +1 @@".into(),
                        "-old".into(),
                        "+new".into(),
                    ],
                    rationale: String::new(),
                },
                ChatItem::StreamingAssistant {
                    text: "done".into(),
                },
            ],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };

        let blocks = model.semantic_blocks();
        assert!(
            matches!(
                blocks.as_slice(),
                [
                    ConversationBlock::UserMessage(_),
                    ConversationBlock::AssistantAnswer(a),
                    ConversationBlock::ActivityGroup(_),
                    ConversationBlock::AssistantAnswer(b),
                    ConversationBlock::DiffBlock(diff),
                    ConversationBlock::AssistantAnswer(c),
                ] if a.text == "I’ll edit foo.rs"
                    && !a.streaming
                    && b.text == "now a test"
                    && !b.streaming
                    && diff.path == "src/foo.rs"
                    && c.text == "done"
                    && c.streaming
            ),
            "expected event order, got {blocks:?}"
        );
    }

    #[test]
    fn thinking_and_plan_stay_in_item_order() {
        let model = ConversationModel {
            items: vec![
                ChatItem::User {
                    text: "plan the change".into(),
                },
                ChatItem::Thinking {
                    text: "Need a checklist first.".into(),
                    duration_secs: Some(1.2),
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
                ChatItem::PlanChecklist {
                    explanation: Some("Next steps".into()),
                    steps: vec![forge_types::PlanItem {
                        step: "Inspect code".into(),
                        status: forge_types::PlanStepStatus::Completed,
                    }],
                },
                ChatItem::Assistant {
                    text: "Ready.".into(),
                },
            ],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };

        let blocks = model.semantic_blocks();
        assert!(
            matches!(
                blocks.as_slice(),
                [
                    ConversationBlock::UserMessage(_),
                    ConversationBlock::Thinking(_),
                    ConversationBlock::ActivityGroup(_),
                    ConversationBlock::PlanChecklist(_),
                    ConversationBlock::AssistantAnswer(a),
                ] if a.text == "Ready."
            ),
            "expected thinking/plan in item order, got {blocks:?}"
        );
    }

    #[test]
    fn routine_reads_do_not_merge_across_assistant_narration() {
        let model = ConversationModel {
            items: group_routine_activity(vec![
                ChatItem::ToolCard {
                    name: "read_file".into(),
                    summary: "a.rs · 1 line".into(),
                    detail: "a.rs".into(),
                    state: ToolCardState::Done,
                    duration: None,
                    subcommand: None,
                    outcome: forge_types::ExecutionOutcome::Success,
                },
                ChatItem::Assistant {
                    text: "looking further".into(),
                },
                ChatItem::ToolCard {
                    name: "read_file".into(),
                    summary: "b.rs · 1 line".into(),
                    detail: "b.rs".into(),
                    state: ToolCardState::Done,
                    duration: None,
                    subcommand: None,
                    outcome: forge_types::ExecutionOutcome::Success,
                },
            ]),
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };

        let blocks = model.semantic_blocks();
        assert!(
            matches!(
                blocks.as_slice(),
                [
                    ConversationBlock::ActivityGroup(_),
                    ConversationBlock::AssistantAnswer(_),
                    ConversationBlock::ActivityGroup(_),
                ]
            ),
            "assistant must break the routine-activity streak, got {blocks:?}"
        );
    }

    #[test]
    fn failure_banner_stays_after_interleaved_answer() {
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
                ChatItem::Assistant {
                    text: "retrying".into(),
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
        assert!(
            matches!(
                blocks.as_slice(),
                [
                    ConversationBlock::UserMessage(_),
                    ConversationBlock::ActivityGroup(_),
                    ConversationBlock::AssistantAnswer(a),
                    ConversationBlock::Callout(c),
                ] if a.text == "retrying"
                    && matches!(c.kind, BannerKind::Error)
                    && c.text.contains("couldn't complete")
            ),
            "failure must stay in event order, got {blocks:?}"
        );
    }

    #[test]
    fn duplicate_turn_failure_banners_keep_the_last() {
        let model = ConversationModel {
            items: vec![
                ChatItem::User { text: "go".into() },
                ChatItem::Banner {
                    text: "first failure".into(),
                    kind: BannerKind::Error,
                },
                ChatItem::Assistant {
                    text: "still trying".into(),
                },
                ChatItem::Banner {
                    text: "final failure".into(),
                    kind: BannerKind::Error,
                },
            ],
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts::default(),
        };
        let blocks = model.semantic_blocks();
        let errors: Vec<_> = blocks
            .iter()
            .filter_map(|block| match block {
                ConversationBlock::Callout(c) if matches!(c.kind, BannerKind::Error) => {
                    Some(c.text.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(errors, vec!["final failure"]);
        assert!(
            matches!(
                blocks.as_slice(),
                [
                    ConversationBlock::UserMessage(_),
                    ConversationBlock::AssistantAnswer(_),
                    ConversationBlock::Callout(_),
                ]
            ),
            "last failure stays after the answer, got {blocks:?}"
        );
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
                attachments: Vec::new(),
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
                attachments: Vec::new(),
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
    fn live_progress_thinking_is_not_appended_after_the_answer() {
        let messages = vec![
            Message {
                outcome: Default::default(),
                role: MessageRole::User,
                content: "hi".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
                attachments: Vec::new(),
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::Assistant,
                content: "final answer".into(),
                tool_call_id: None,
                name: None,
                thinking: Some("reason first".into()),
                thinking_duration_secs: None,
                tool_calls: vec![],
                attachments: Vec::new(),
            },
        ];
        let events = vec![forge_core::TurnEvent {
            kind: "progress".into(),
            detail: "stale late thought".into(),
        }];
        let model = ConversationModel::from_messages(
            &messages,
            &events,
            TaskLifecycle::Working,
            ConversationViewOpts::default(),
        );
        let blocks = model.semantic_blocks();
        assert!(
            matches!(
                blocks.as_slice(),
                [
                    ConversationBlock::UserMessage(_),
                    ConversationBlock::Thinking(t),
                    ConversationBlock::AssistantAnswer(a),
                ] if t.text == "reason first" && a.text == "final answer"
            ),
            "thinking must stay above the answer, got {blocks:?}"
        );
        assert!(
            !blocks.iter().any(|block| match block {
                ConversationBlock::Thinking(t) => t.text.contains("stale late thought"),
                _ => false,
            }),
            "progress events must not append thinking after the answer: {blocks:?}"
        );
    }

    #[test]
    fn streaming_preview_places_thinking_before_the_answer() {
        let model = ConversationModel::from_messages(
            &[],
            &[],
            TaskLifecycle::Working,
            ConversationViewOpts {
                busy: true,
                stream_thought_secs: Some(1.5),
                ..Default::default()
            },
        )
        .with_streaming_preview("planning the edit", "partial reply");
        let blocks = model.semantic_blocks();
        assert!(
            matches!(
                blocks.as_slice(),
                [
                    ConversationBlock::Thinking(t),
                    ConversationBlock::AssistantAnswer(a),
                ] if t.text == "planning the edit"
                    && t.duration_secs == Some(1.5)
                    && a.text.contains("partial reply")
                    && a.streaming
            ),
            "live thinking must precede the streaming answer, got {blocks:?}"
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
    fn turn_failed_marker_is_hidden_from_transcript() {
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
                attachments: Vec::new(),
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
                attachments: Vec::new(),
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
            !blocks.iter().any(|block| matches!(
                block,
                ConversationBlock::AssistantAnswer(_) | ConversationBlock::Callout(_)
            )),
            "failure marker must not render in the transcript: {blocks:?}"
        );
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
                attachments: Vec::new(),
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
                attachments: Vec::new(),
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
                attachments: Vec::new(),
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
                attachments: Vec::new(),
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
            attachments: Vec::new(),
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
                    name: "grep".into(),
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
    fn classify_view_image_summarizes_path_and_omits_bytes() {
        let call = ToolCall {
            id: "img-1".into(),
            name: "view_image".into(),
            arguments: serde_json::json!({"path": "docs/shot.png"}),
        };
        let (state, summary, invocation, detail) = classify_tool_content(
            "view_image",
            "image loaded · 12 KB · docs/shot.png · 80×40",
            Some(&call),
            &ExecutionOutcome::Success,
        );
        assert_eq!(state, ToolCardState::Done);
        assert!(summary.contains("docs/shot.png"));
        assert!(!summary.contains("data:"));
        assert_eq!(invocation.as_deref(), Some("docs/shot.png"));
        assert!(!detail.contains("data:"));
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
                attachments: Vec::new(),
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
                attachments: Vec::new(),
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
                attachments: Vec::new(),
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
            attachments: Vec::new(),
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
            attachments: Vec::new(),
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
}

#[cfg(test)]
mod spent_reasoning_tests {
    use super::*;

    fn thinking(text: &str, secs: f64) -> ChatItem {
        ChatItem::Thinking {
            text: text.into(),
            duration_secs: Some(secs),
        }
    }

    fn model(items: Vec<ChatItem>, tool_expanded: bool) -> ConversationModel {
        ConversationModel {
            items,
            scroll: 0,
            follow: true,
            opts: ConversationViewOpts {
                tool_expanded,
                ..ConversationViewOpts::default()
            },
        }
    }

    /// Reasoning used to be permanent: a turn that ran three tools left three
    /// dim paragraphs behind. Only the newest stays open.
    #[test]
    fn only_the_newest_reasoning_stays_open() {
        let blocks = model(
            vec![
                thinking("first pass", 1.0),
                thinking("second pass", 2.0),
                thinking("current", 3.0),
            ],
            false,
        )
        .semantic_blocks();
        let collapsed: Vec<bool> = blocks
            .iter()
            .filter_map(|block| match block {
                ConversationBlock::Thinking(t) => Some(t.collapsed),
                _ => None,
            })
            .collect();
        assert_eq!(collapsed, vec![true, true, false]);
    }

    /// Ctrl+O is the existing "show me everything" affordance, so it has to
    /// bring the spent reasoning back too.
    #[test]
    fn expanding_restores_every_reasoning_block() {
        let blocks = model(
            vec![thinking("first pass", 1.0), thinking("current", 3.0)],
            true,
        )
        .semantic_blocks();
        assert!(blocks.iter().all(|block| !matches!(
            block,
            ConversationBlock::Thinking(t) if t.collapsed
        )));
    }

    /// A single reasoning block is the newest one, so nothing collapses.
    #[test]
    fn a_lone_reasoning_block_is_never_collapsed() {
        let blocks = model(vec![thinking("only", 1.0)], false).semantic_blocks();
        assert!(matches!(
            blocks.first(),
            Some(ConversationBlock::Thinking(t)) if !t.collapsed
        ));
    }
}
