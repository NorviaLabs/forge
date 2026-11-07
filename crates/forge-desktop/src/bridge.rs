//! The IPC bridge: Forge's supervisor on one side, the webview on the other.
//!
//! Provisional. The event shape here is the walking skeleton's own and is
//! not frozen; phase P replaces it with the versioned `forge-app` protocol
//! (revision-keyed deltas instead of the whole transcript per update).

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::State;

use forge_session::{
    RepositorySession, SessionLifecycle, SessionRuntimeSnapshot, SupervisorCommand,
    SupervisorEvent, SupervisorHandle,
};
use forge_types::{
    ApprovalKind, AskUserQuestionAnswerItem, AskUserQuestionResult, GrantDecision, HitlDecision,
    HitlPayload, MessageRole, SessionId,
};

/// Tool output longer than this is cut in the transcript view.
const TOOL_TEXT_LIMIT: usize = 2000;
const THINKING_TEXT_LIMIT: usize = 4000;

/// What the webview receives on its channel.
///
/// Events are built once per update and serialized immediately, so the size
/// skew between the whole-transcript `Session` frame and the small ones costs
/// one move, not a copy.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DesktopEvent {
    /// Streamed model output, passed through as the core emits it.
    Stream {
        session_id: SessionId,
        event: serde_json::Value,
    },
    /// The session as it stands: turn state, transcript and any approval
    /// waiting on the user.
    Session {
        session_id: SessionId,
        turn_state: &'static str,
        messages: Vec<MessageView>,
        pending_approval: Option<ApprovalView>,
        /// The outstanding `ask_user_question` call, if the turn is parked on
        /// one. Without it a question-waiting turn looks idle forever.
        pending_question: Option<QuestionView>,
        /// Per-session approve-all: review prompts auto-approved, the sandbox
        /// still enforced.
        approve_all: bool,
        thinking: bool,
        /// The last `update_plan` checklist the agent published.
        plan: Option<PlanView>,
        tokens: TokenView,
        /// Background shell jobs and subagents, with waits and terminal states.
        background: Vec<BackgroundView>,
        /// Prompts waiting for the turn to finish.
        queue: Vec<QueueView>,
        /// Prompts recovered from a stopped or interrupted turn.
        interrupted: Vec<QueueView>,
        tools: usize,
        skills: usize,
        pattern_allows: usize,
        image_input: bool,
        /// The active session's workspace, branch and model, for the
        /// inspector. No credentials.
        workspace: String,
        branch: String,
        model: String,
        /// 0.0..=1.0 of the context window in use.
        context_usage: f64,
        queued: usize,
        /// The issue this session was created for, when it is issue-linked.
        github_issue_number: Option<u64>,
        github_pr_url: Option<String>,
    },
    /// The sessions the supervisor owns, in switcher order.
    Roster {
        sessions: Vec<SessionRow>,
    },
    /// The session the prompt and approval commands target. The following
    /// `Session` event carries its transcript.
    Selected {
        session_id: Option<SessionId>,
    },
    /// The session needs the user's attention.
    Attention {
        session_id: SessionId,
        turn_state: &'static str,
        message: String,
    },
    Error {
        message: String,
    },
    /// The broadcast dropped events for a slow consumer; the view must
    /// treat its state as stale.
    Lagged {
        missed: u64,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MessageView {
    pub role: &'static str,
    /// The tool name for tool messages; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Markdown for user and assistant messages; plain text for tools.
    pub text: String,
    /// Assistant reasoning, when the provider exposed any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// Images this message carried (a pasted prompt or a `view_image` result).
    /// Paths only; the page asks for the bytes when it shows one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentView>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AttachmentView {
    pub path: String,
    pub mime: String,
    pub byte_len: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

fn attachment_views(attachments: &[forge_types::ImageRef]) -> Vec<AttachmentView> {
    attachments
        .iter()
        .map(|image| AttachmentView {
            path: escape_invisible(&image.path),
            mime: image.mime.clone(),
            byte_len: image.byte_len,
            width: image.width,
            height: image.height,
        })
        .collect()
}

/// One row in the session switcher. No workspace path, branch, model or
/// transcript: the sidebar only needs to name and select a session.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SessionRow {
    pub session_id: SessionId,
    pub label: String,
    pub turn_state: &'static str,
    /// The pinned slot, when the session is pinned.
    pub slot: Option<u8>,
    /// `active`, `archived` or `retained`. Archived and retained rows are
    /// listed so the operator can resume them; they are not selectable.
    pub lifecycle: &'static str,
    /// `primary`, `managed` or `attached`. Only a managed worktree can be
    /// removed with the session; an attached one belongs to the operator.
    pub ownership: &'static str,
}

fn ownership_label(ownership: forge_session::WorktreeOwnership) -> &'static str {
    match ownership {
        forge_session::WorktreeOwnership::Primary => "primary",
        forge_session::WorktreeOwnership::Managed => "managed",
        forge_session::WorktreeOwnership::Attached => "attached",
    }
}

fn session_row(task: &RepositorySession) -> SessionRow {
    SessionRow {
        session_id: task.session_id,
        label: escape_invisible(&task.label),
        turn_state: task.turn_state.label(),
        slot: task.slot,
        lifecycle: match task.lifecycle {
            SessionLifecycle::Active => "active",
            SessionLifecycle::Archived => "archived",
            SessionLifecycle::Retained => "retained",
            SessionLifecycle::Unavailable => "unavailable",
            SessionLifecycle::Removed => "removed",
        },
        ownership: ownership_label(task.ownership),
    }
}

/// The switcher's rows: every session the supervisor still knows about.
/// Archived ones carry their lifecycle so the sidebar can list them under
/// "Closed" and offer resume; `select_session` only targets active ones.
pub fn session_rows(snapshots: &[SessionRuntimeSnapshot]) -> Vec<SessionRow> {
    snapshots
        .iter()
        .map(|snapshot| session_row(&snapshot.task))
        .collect()
}

/// Approvals whose every affirmative answer needs the native confirmation
/// (AC-4.12): running unconfined, or opening a network host.
pub fn requires_native_confirm(payload: &HitlPayload) -> bool {
    matches!(
        payload.kind(),
        ApprovalKind::SandboxEscalation | ApprovalKind::NetworkHost
    )
}

/// Why the app refused to forward an approval decision.
#[derive(Debug, PartialEq)]
pub enum Refusal {
    NoApprovalId,
    /// Nothing is pending, or the pending request is a different one.
    NotPending,
    /// Approve on a privileged request; only deny is possible from the page.
    NeedsNativeConfirm,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refusal::NoApprovalId => "no approval id",
            Refusal::NotPending => "that request is no longer pending",
            Refusal::NeedsNativeConfirm => {
                "this request needs a native confirmation, which arrives in a later release: \
                 approve it in the terminal, or deny it here"
            }
        })
    }
}

/// Decide, against the request actually pending, whether the page's
/// decision may be forwarded. Deny is always allowed for the pending
/// request; Approve is refused for privileged requests.
pub fn check_decision(
    pending: Option<&HitlPayload>,
    call_id: &str,
    decision: &HitlDecision,
) -> Result<(), Refusal> {
    if call_id.trim().is_empty() {
        return Err(Refusal::NoApprovalId);
    }
    let Some(pending) = pending.filter(|payload| payload.call_id == call_id) else {
        return Err(Refusal::NotPending);
    };
    if matches!(decision, HitlDecision::Approve) && requires_native_confirm(pending) {
        return Err(Refusal::NeedsNativeConfirm);
    }
    Ok(())
}

/// One decision the approval card offers. The backend, not the page, decides
/// which grants exist for a request; the page only names one.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ApprovalOption {
    /// The `GrantDecision` name sent back to `decide_approval`.
    pub decision: String,
    pub label: String,
    /// The rule this would remember, when the option grants one.
    pub detail: String,
    pub help: String,
    /// Needs the card's second confirmation step: it runs unconfined or opens
    /// a host.
    pub privileged: bool,
}

fn option(
    decision: &str,
    label: String,
    detail: &str,
    help: &str,
    privileged: bool,
) -> ApprovalOption {
    ApprovalOption {
        decision: decision.into(),
        label,
        detail: detail.into(),
        help: help.into(),
        privileged,
    }
}

/// The grants this request can carry, in the order the card shows them.
///
/// Mirrors the terminal's menu: a denied host is granted for the session at
/// least (running it once would only meet the same denial), a sandbox
/// escalation always runs outside the sandbox, and approve-all is never
/// offered for one.
pub fn approval_options(payload: &HitlPayload) -> Vec<ApprovalOption> {
    let mut options = Vec::new();
    match payload.kind() {
        ApprovalKind::NetworkHost => {
            let host = payload.denied_host.as_deref().unwrap_or("this host");
            options.push(option(
                "allow_host_for_session",
                format!("Allow {host} this session"),
                &format!("host({host})"),
                "The sandbox stays on; the call reruns confined. You will be asked again next session.",
                true,
            ));
            options.push(option(
                "allow_host_always",
                format!("Always allow {host}"),
                &format!("host({host})"),
                "Writes the rule to your personal permissions file. Kept next session.",
                true,
            ));
            options.push(option(
                "approve_all",
                "Trust all commands for this session".into(),
                "",
                "Approves review prompts and network requests until the session ends. The sandbox stays on.",
                true,
            ));
        }
        ApprovalKind::SandboxEscalation => {
            options.push(option(
                "allow_once",
                "Approve command".into(),
                "",
                "Retries this command outside the filesystem sandbox. That can run destructive commands.",
                true,
            ));
            options.push(option(
                "allow_pattern_for_session",
                "Allow this kind of call outside the sandbox this session".into(),
                "",
                "Later matching calls run unconfined until the session ends. Never saved.",
                true,
            ));
        }
        ApprovalKind::Governance => {
            options.push(option(
                "allow_once",
                "Run once".into(),
                "",
                "Runs now. You will be asked again.",
                false,
            ));
            options.push(option(
                "allow_pattern_for_session",
                "Allow this kind of call this session".into(),
                "",
                "The sandbox stays on. You will be asked again next session.",
                false,
            ));
            options.push(option(
                "allow_pattern_always",
                "Always allow this kind of call".into(),
                "",
                "Writes the rule to your personal permissions file. Kept next session.",
                false,
            ));
            options.push(option(
                "approve_all",
                "Trust all commands for this session".into(),
                "",
                "Approves review prompts and network requests until the session ends. The sandbox stays on.",
                false,
            ));
        }
    }
    options.push(option(
        "deny",
        "Don't run".into(),
        "",
        "The agent is told the call was denied.",
        false,
    ));
    options.push(option(
        "deny_with_note",
        "Don't run, and say why".into(),
        "",
        "The note reaches the agent as the tool result.",
        false,
    ));
    options
}

/// A pending approval as the page shows it. `call_id` is issued by the
/// backend and is the only thing the page can approve or deny.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ApprovalView {
    pub call_id: String,
    pub tool: String,
    /// The arguments, pretty-printed, with invisible and bidi-control
    /// characters made visible.
    pub arguments: String,
    pub reason: String,
    /// A sandbox escalation or network grant: every approving option needs the
    /// card's second confirmation step.
    pub privileged: bool,
    pub kind: &'static str,
    pub options: Vec<ApprovalOption>,
    /// Set for a background child request: the exact request this view
    /// describes, so a decision cannot land on a replacement request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// One `ask_user_question` call waiting on the operator.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct QuestionView {
    pub call_id: String,
    pub questions: Vec<QuestionItemView>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct QuestionItemView {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOptionView>,
    pub multi_select: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct QuestionOptionView {
    pub label: String,
    pub value: String,
    pub description: String,
}

/// One `update_plan` row.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlanItemView {
    pub step: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlanView {
    pub explanation: String,
    pub items: Vec<PlanItemView>,
}

/// API-reported token totals and context projection, for the status view.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct TokenView {
    pub prompt: u64,
    pub completion: u64,
    pub cache_hits: u64,
    pub cache_writes: u64,
    pub context_tokens: usize,
    pub context_capacity: usize,
    pub context_pct: f64,
    pub tool_schema_tokens: usize,
    pub message_count: usize,
    pub model_steps: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BackgroundView {
    pub id: u64,
    /// Identifies this execution of the task, so cancelling cannot stop a
    /// replacement that reused the same task id.
    pub run_id: String,
    pub label: String,
    /// `shell` or `subagent`.
    pub kind: String,
    /// `queued`, `running`, `waiting_for_approval`, `succeeded`, `failed` or
    /// `cancelled`.
    pub status: String,
    /// The command, role, summary or error behind the row.
    pub detail: String,
    pub latest_message: String,
    pub awaiting_approval: bool,
    pub child_session_id: Option<SessionId>,
    pub approval: Option<ApprovalView>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct QueueView {
    pub id: u64,
    pub source: QueueSource,
    pub text: String,
}

#[derive(Debug, Clone, Copy, Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum QueueSource {
    Durable,
    Session,
}

fn queue_views(snapshot: &SessionRuntimeSnapshot) -> Vec<QueueView> {
    snapshot
        .queued_prompts
        .iter()
        .map(|(id, text)| QueueView {
            id: *id,
            source: QueueSource::Durable,
            text: escape_invisible(text),
        })
        .chain(
            snapshot
                .details
                .iter()
                .flat_map(|details| &details.queue)
                .map(|task| QueueView {
                    id: task.id.0,
                    source: QueueSource::Session,
                    text: escape_invisible(&task.text),
                }),
        )
        .collect()
}

fn question_view(payload: &forge_types::QuestionPayload) -> QuestionView {
    QuestionView {
        call_id: payload.call_id.clone(),
        questions: payload
            .questions
            .iter()
            .map(|question| QuestionItemView {
                id: question.id.clone(),
                header: escape_invisible(&question.header),
                question: escape_invisible(&question.question),
                options: question
                    .options
                    .iter()
                    .map(|option| QuestionOptionView {
                        label: escape_invisible(&option.label),
                        value: option.label.clone(),
                        description: escape_invisible(&option.description),
                    })
                    .collect(),
                multi_select: question.multi_select,
            })
            .collect(),
    }
}

/// The newest `update_plan` payload in the transcript, if there is one.
fn plan_view(snapshot: &SessionRuntimeSnapshot) -> Option<PlanView> {
    let event = snapshot
        .transcript
        .events()
        .iter()
        .rev()
        .find(|event| event.kind == "plan_update")?;
    let args: forge_types::UpdatePlanArgs = serde_json::from_str(&event.detail).ok()?;
    Some(PlanView {
        explanation: escape_invisible(args.explanation.as_deref().unwrap_or("")),
        items: args
            .plan
            .iter()
            .map(|item| PlanItemView {
                step: escape_invisible(&item.step),
                status: item.status.as_str().to_string(),
            })
            .collect(),
    })
}

fn background_views(
    details: Option<&forge_session::SessionDetailsSnapshot>,
) -> Vec<BackgroundView> {
    use forge_core::{BackgroundTaskKind, BackgroundTaskStatus};
    details
        .map(|details| details.background.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|task| {
            let (status, detail, awaiting_approval) = match &task.status {
                BackgroundTaskStatus::Queued => ("queued", String::new(), false),
                BackgroundTaskStatus::Running => ("running", String::new(), false),
                BackgroundTaskStatus::WaitingForApproval { payload } => {
                    ("waiting_for_approval", payload.reason.clone(), true)
                }
                BackgroundTaskStatus::Succeeded { summary } => {
                    ("succeeded", summary.clone(), false)
                }
                BackgroundTaskStatus::Failed { error } => ("failed", error.clone(), false),
                BackgroundTaskStatus::Cancelled => ("cancelled", String::new(), false),
            };
            let (kind, subject) = match &task.kind {
                BackgroundTaskKind::Shell { command } => ("shell", command.clone()),
                BackgroundTaskKind::Subagent { role, prompt } => {
                    ("subagent", format!("{role}: {prompt}"))
                }
            };
            BackgroundView {
                id: task.id.0,
                run_id: task.run_id.to_string(),
                label: escape_invisible(&task.label),
                kind: kind.into(),
                status: status.into(),
                detail: escape_invisible(if detail.is_empty() { &subject } else { &detail }),
                latest_message: escape_invisible(task.latest_message.as_deref().unwrap_or("")),
                awaiting_approval,
                child_session_id: task.child_session_id,
                approval: task.pending_approval.as_ref().map(child_approval_view),
            }
        })
        .collect()
}

fn token_view(details: Option<&forge_session::SessionDetailsSnapshot>) -> TokenView {
    details
        .map(|details| details.token_usage_report.clone())
        .map(|report| TokenView {
            prompt: report.api.prompt_tokens,
            completion: report.api.completion_tokens,
            cache_hits: report.api.prompt_cache_hits,
            cache_writes: report.api.prompt_cache_writes,
            context_tokens: report.context_tokens_est,
            context_capacity: report.context_capacity,
            context_pct: report.context_pct,
            tool_schema_tokens: report.tool_schema_tokens_est,
            message_count: report.message_count,
            model_steps: report.api.model_steps,
        })
        .unwrap_or_default()
}

/// Make characters that can disguise what a command does visible:
/// bidi controls, zero-width characters and the byte-order mark become
/// `⟨U+XXXX⟩`.
pub fn escape_invisible(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}' => format!("⟨U+{:04X}⟩", c as u32),
            c => c.to_string(),
        })
        .collect()
}

fn approval_view(payload: &HitlPayload) -> ApprovalView {
    let arguments = serde_json::to_string_pretty(&payload.args_redacted)
        .unwrap_or_else(|_| payload.args_redacted.to_string());
    ApprovalView {
        call_id: payload.call_id.clone(),
        tool: escape_invisible(&payload.tool),
        arguments: escape_invisible(&arguments),
        reason: escape_invisible(&payload.reason),
        privileged: requires_native_confirm(payload),
        kind: match payload.kind() {
            ApprovalKind::NetworkHost => "network_host",
            ApprovalKind::SandboxEscalation => "sandbox_escalation",
            ApprovalKind::Governance => "governance",
        },
        options: approval_options(payload),
        request_id: None,
    }
}

fn child_approval_view(request: &forge_core::BackgroundApprovalRequest) -> ApprovalView {
    let mut view = approval_view(&request.payload);
    view.request_id = Some(request.id.to_string());
    view
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_string()
    } else {
        text.chars().take(limit).collect::<String>() + "\n…"
    }
}

/// The IPC shape of one transcript message. `full` skips the snapshot limits;
/// the detail command re-reads a single message when the operator asks.
fn message_view(message: &forge_types::Message, full: bool) -> Option<MessageView> {
    let role = match message.role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
        _ => return None,
    };
    let text = if full {
        message.content.clone()
    } else if message.role == MessageRole::Tool {
        truncate(&message.content, TOOL_TEXT_LIMIT)
    } else {
        message.content.clone()
    };
    let thinking = message.thinking.as_ref().map(|thinking| {
        if full {
            thinking.clone()
        } else {
            truncate(thinking, THINKING_TEXT_LIMIT)
        }
    });
    Some(MessageView {
        role,
        name: message.name.clone(),
        text,
        thinking,
        attachments: attachment_views(&message.attachments),
    })
}

fn session_event(snapshot: &SessionRuntimeSnapshot) -> DesktopEvent {
    let messages = snapshot
        .transcript
        .messages()
        .iter()
        .filter_map(|message| message_view(message, false))
        .collect();
    let pending_approval = snapshot.session.pending_hitl.as_ref().map(approval_view);
    DesktopEvent::Session {
        session_id: snapshot.task.session_id,
        turn_state: snapshot.task.turn_state.label(),
        messages,
        pending_approval,
        pending_question: snapshot
            .session
            .pending_question
            .as_ref()
            .map(question_view),
        approve_all: snapshot.session.approve_all,
        thinking: snapshot
            .details
            .as_ref()
            .is_some_and(|details| details.thinking_enabled),
        plan: plan_view(snapshot),
        tokens: token_view(snapshot.details.as_ref()),
        background: background_views(snapshot.details.as_ref()),
        queue: queue_views(snapshot),
        interrupted: snapshot
            .interrupted_prompts
            .iter()
            .map(|(id, text)| QueueView {
                id: *id,
                source: QueueSource::Session,
                text: escape_invisible(text),
            })
            .collect(),
        tools: snapshot.session.tool_count,
        skills: snapshot.session.loaded_skills_count,
        pattern_allows: snapshot
            .details
            .as_ref()
            .map(|details| details.session_pattern_allow_count)
            .unwrap_or_default(),
        image_input: snapshot
            .details
            .as_ref()
            .is_some_and(|details| details.image_input_supported),
        workspace: snapshot.task.workspace.display().to_string(),
        branch: snapshot.task.branch.clone(),
        model: snapshot
            .details
            .as_ref()
            .map(|details| details.active_model.clone())
            .unwrap_or_default(),
        context_usage: snapshot.session.context_usage_ratio,
        queued: snapshot.session.queue_len + snapshot.queued_prompts.len(),
        github_issue_number: snapshot.task.github_issue_number,
        github_pr_url: snapshot.task.github_pr_url.clone(),
    }
}

/// Translate one supervisor event. `None` for events the skeleton does not
/// show yet.
pub fn translate(event: SupervisorEvent) -> Option<DesktopEvent> {
    match event {
        SupervisorEvent::Stream { session_id, event } => Some(DesktopEvent::Stream {
            session_id,
            event: serde_json::to_value(event).ok()?,
        }),
        SupervisorEvent::SessionUpdated(snapshot) => Some(session_event(&snapshot)),
        SupervisorEvent::Roster(snapshots) => Some(DesktopEvent::Roster {
            sessions: session_rows(&snapshots),
        }),
        SupervisorEvent::Selected(session_id) => Some(DesktopEvent::Selected { session_id }),
        SupervisorEvent::Attention {
            session_id,
            state,
            message,
        } => Some(DesktopEvent::Attention {
            session_id,
            turn_state: state.label(),
            message,
        }),
        SupervisorEvent::Error { message, .. } => Some(DesktopEvent::Error { message }),
        _ => None,
    }
}

/// Process-wide state the commands share.
pub struct DesktopState {
    pub handle: SupervisorHandle,
    /// Read for the live session state; taken (write) only at exit to shut
    /// the supervisor down.
    pub supervisor:
        std::sync::Arc<tokio::sync::RwLock<Option<forge_session::RepositorySupervisor>>>,
    pub session_id: SessionId,
    /// The session as it was when the window opened, so the page can show
    /// the transcript before the first update arrives.
    pub initial: DesktopEvent,
    /// Live sessions as they were when the window opened.
    pub sessions: Vec<SessionRow>,
    /// The session's interactive shells (one per session, opened on demand).
    pub shells: crate::shell::Shells,
    /// The page has unsaved editor buffers or live turns; a close asks first.
    pub unsaved: std::sync::atomic::AtomicBool,
    /// A turn is running or waiting somewhere, so quitting discards work.
    pub busy: std::sync::atomic::AtomicBool,
    /// The user confirmed quitting despite unsaved buffers.
    pub quitting: std::sync::atomic::AtomicBool,
    #[cfg(feature = "self-test")]
    pub self_test: Option<std::sync::Arc<crate::self_test::Probe>>,
}

impl DesktopState {
    pub fn new(
        handle: SupervisorHandle,
        supervisor: std::sync::Arc<
            tokio::sync::RwLock<Option<forge_session::RepositorySupervisor>>,
        >,
        session_id: SessionId,
        initial: &SessionRuntimeSnapshot,
        sessions: Vec<SessionRow>,
    ) -> Self {
        Self {
            handle,
            supervisor,
            session_id,
            initial: session_event(initial),
            sessions,
            shells: crate::shell::Shells::default(),
            unsaved: std::sync::atomic::AtomicBool::new(false),
            busy: std::sync::atomic::AtomicBool::new(false),
            quitting: std::sync::atomic::AtomicBool::new(false),
            #[cfg(feature = "self-test")]
            self_test: None,
        }
    }
}

/// Whether the window should show the trust screen. Present in every build:
/// the page asks before it subscribes.
pub struct TrustState {
    pub workspace: std::path::PathBuf,
    /// `Some` until the operator trusts the workspace and a session starts.
    deferred: std::sync::Mutex<Option<forge_config::Config>>,
    /// Why starting after trust failed, so a retry can say what happened.
    error: std::sync::Mutex<String>,
}

impl TrustState {
    pub fn ready(workspace: std::path::PathBuf) -> Self {
        Self {
            workspace,
            deferred: std::sync::Mutex::new(None),
            error: std::sync::Mutex::new(String::new()),
        }
    }

    pub fn pending(workspace: std::path::PathBuf, deferred: forge_config::Config) -> Self {
        Self {
            deferred: std::sync::Mutex::new(Some(deferred)),
            ..Self::ready(workspace)
        }
    }

    fn restore(&self, deferred: forge_config::Config) {
        if let Ok(mut slot) = self.deferred.lock() {
            *slot = Some(deferred);
        }
    }

    fn fail(&self, message: String) -> String {
        if let Ok(mut error) = self.error.lock() {
            *error = message.clone();
        }
        message
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TrustView {
    pub workspace: String,
    pub needs_trust: bool,
    pub error: String,
}

/// The page asks this first. `needs_trust` means: show the trust screen and
/// do not subscribe yet.
#[tauri::command]
pub fn trust_status(state: State<'_, TrustState>) -> TrustView {
    trust_view(&state)
}

pub fn trust_view(state: &TrustState) -> TrustView {
    TrustView {
        workspace: state.workspace.display().to_string(),
        needs_trust: state
            .deferred
            .lock()
            .map(|slot| slot.is_some())
            .unwrap_or(false),
        error: state
            .error
            .lock()
            .map(|error| error.clone())
            .unwrap_or_default(),
    }
}

/// Record trust for the workspace, then open the session that was held back.
///
/// Trust is written to the same store `forge` reads, so the terminal and the
/// desktop agree from here on. The plan is re-prepared afterwards: `launch`
/// re-reads trust from disk, and this is the check that would catch a store
/// that refused the write.
#[tauri::command]
pub async fn trust_workspace(
    app: tauri::AppHandle,
    state: State<'_, TrustState>,
) -> Result<(), String> {
    use tauri::Manager;
    let Some(deferred) = state.deferred.lock().ok().and_then(|mut slot| slot.take()) else {
        return Err("this workspace is already trusted".into());
    };
    if let Err(error) = forge_config::grant_trust(&state.workspace) {
        state.restore(deferred);
        return Err(state.fail(format!("cannot record trust: {error}")));
    }
    let plan = match forge_session::LaunchPlan::prepare(&deferred).await {
        Ok(plan) => plan,
        Err(error) => {
            state.restore(deferred);
            return Err(state.fail(error.to_string()));
        }
    };
    let started = match crate::startup::open(deferred.clone(), plan, None).await {
        Ok(started) => started,
        Err(error) => {
            state.restore(deferred);
            return Err(state.fail(format!("{error:#}")));
        }
    };
    let supervisor = std::sync::Arc::new(tokio::sync::RwLock::new(Some(started.supervisor)));
    app.manage(DesktopState::new(
        started.handle,
        supervisor,
        started.session_id,
        &started.initial,
        started.sessions,
    ));
    if let Ok(mut error) = state.error.lock() {
        error.clear();
    }
    Ok(())
}

/// What `subscribe` returns before events start flowing.
#[derive(Debug, Serialize)]
pub struct Hello {
    pub session_id: SessionId,
    pub initial: DesktopEvent,
    pub sessions: Vec<SessionRow>,
    /// Self-test builds only: the probe's parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_test: Option<serde_json::Value>,
}

/// Stream supervisor events into `channel` until the window goes away.
#[tauri::command]
pub fn subscribe(state: State<'_, DesktopState>, channel: Channel<DesktopEvent>) -> Hello {
    let mut events = state.handle.subscribe();
    let supervisor = state.supervisor.clone();
    #[cfg(feature = "self-test")]
    let probe = state.self_test.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => translate(event),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    Some(DesktopEvent::Lagged { missed })
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let Some(event) = event else { continue };
            #[cfg(feature = "self-test")]
            if let Some(probe) = &probe {
                probe.delivering(&event);
            }
            let selected = match &event {
                DesktopEvent::Selected { session_id } => Some(*session_id),
                _ => None,
            };
            if channel.send(event).is_err() {
                // The webview is gone.
                break;
            }
            // Selecting emits only the id, but the page needs that session's
            // transcript. Send it right after the selection.
            if let Some(session_id) = selected {
                if let Some(snapshot) = selected_snapshot(&supervisor, session_id).await {
                    #[cfg(feature = "self-test")]
                    if let Some(probe) = &probe {
                        probe.delivering(&snapshot);
                    }
                    if channel.send(snapshot).is_err() {
                        break;
                    }
                }
            }
        }
    });
    #[cfg(feature = "self-test")]
    let self_test = state.self_test.as_ref().map(|probe| probe.hello());
    #[cfg(not(feature = "self-test"))]
    let self_test = None;
    Hello {
        session_id: state.session_id,
        initial: state.initial.clone(),
        sessions: state.sessions.clone(),
        self_test,
    }
}

/// The snapshot behind a selection, read from the supervisor's live actors.
async fn selected_snapshot(
    supervisor: &std::sync::Arc<tokio::sync::RwLock<Option<forge_session::RepositorySupervisor>>>,
    session_id: Option<SessionId>,
) -> Option<DesktopEvent> {
    let session_id = session_id?;
    let supervisor = supervisor.read().await;
    let snapshot = supervisor.as_ref()?.snapshot(session_id).await?;
    Some(session_event(&snapshot))
}

/// The session the operator currently has selected; the prompt, stop and
/// approval commands target it.
async fn selected_session(state: &DesktopState) -> Result<SessionId, String> {
    let supervisor = state.supervisor.read().await;
    let Some(supervisor) = supervisor.as_ref() else {
        return Err("Forge is shutting down".into());
    };
    supervisor
        .selected_session_id()
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no session is selected".to_string())
}

#[tauri::command]
pub async fn submit_prompt(
    state: State<'_, DesktopState>,
    text: String,
    attachment: Option<crate::workspace::PromptAttachment>,
) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("prompt is empty".into());
    }
    let session_id = selected_session(&state).await?;
    let text = match attachment {
        Some(attachment) => {
            let root = workspace_of(&state, session_id).await?;
            let context = tokio::task::spawn_blocking(move || {
                crate::workspace::attachment_text(&root, &attachment)
            })
            .await
            .map_err(|error| error.to_string())??;
            format!("{context}\n\n{text}")
        }
        None => text,
    };
    #[cfg(feature = "self-test")]
    if let Some(probe) = &state.self_test {
        probe.submitted();
    }
    state
        .handle
        .submit(SupervisorCommand::SubmitPrompt { session_id, text })
        // Fire and forget: the outcome arrives on the event channel.
        .map(drop)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn stop_turn(state: State<'_, DesktopState>) -> Result<(), String> {
    let session_id = selected_session(&state).await?;
    state
        .handle
        .submit(SupervisorCommand::StopTurn { session_id })
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Image bytes, not page-supplied filesystem paths, enter at this boundary.
#[tauri::command]
pub async fn submit_images(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    text: String,
    images: Vec<String>,
    attachment: Option<crate::workspace::PromptAttachment>,
) -> Result<(), String> {
    if selected_session(&state).await? != session_id {
        return Err("Session changed; paste the image again in the intended session".into());
    }
    let root = {
        let supervisor = state.supervisor.read().await;
        let snapshot = supervisor
            .as_ref()
            .ok_or("Forge is shutting down")?
            .snapshot(session_id)
            .await
            .ok_or("Session is gone")?;
        if !snapshot
            .details
            .as_ref()
            .is_some_and(|details| details.image_input_supported)
        {
            return Err(
                "This model does not support images; choose an image-capable model first".into(),
            );
        }
        snapshot.task.workspace
    };
    let text = match attachment {
        Some(attachment) => {
            let attach_root = root.clone();
            let context = tokio::task::spawn_blocking(move || {
                crate::workspace::attachment_text(&attach_root, &attachment)
            })
            .await
            .map_err(|error| error.to_string())??;
            format!("{context}\n\n{text}")
        }
        None => text,
    };
    let attachments =
        tokio::task::spawn_blocking(move || crate::workspace::store_images(&root, &images))
            .await
            .map_err(|error| error.to_string())??;
    state
        .handle
        .command(SupervisorCommand::SubmitPromptWithAttachments {
            session_id,
            text: if text.trim().is_empty() {
                "Describe this image.".into()
            } else {
                text
            },
            attachments,
        })
        .await
        .map_err(|error| error.to_string())
}

/// Create a session (its own managed worktree) and select it. The roster
/// and selection arrive on the event channel.
#[tauri::command]
pub async fn create_session(
    state: State<'_, DesktopState>,
    label: String,
    github_issue_number: Option<u64>,
) -> Result<(), String> {
    let label = label.trim().to_string();
    if label.is_empty() {
        return Err("session name is empty".into());
    }
    state
        .handle
        .command(SupervisorCommand::CreateSession {
            label,
            first_prompt: None,
            github_issue_number,
        })
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn select_session(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<(), String> {
    state
        .handle
        .command(SupervisorCommand::SelectSession {
            session_id: Some(session_id),
        })
        .await
        .map_err(|error| error.to_string())
}

/// Archive a session. The supervisor picks the next selection and publishes
/// the new roster.
#[tauri::command]
pub async fn archive_session(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<(), String> {
    state
        .handle
        .command(SupervisorCommand::ArchiveSession { session_id })
        .await
        .map_err(|error| error.to_string())
}

/// Attach an existing linked worktree as a session. The supervisor owns the
/// membership and branch checks, so a typo cannot create a stray session.
#[tauri::command]
pub async fn attach_worktree(
    state: State<'_, DesktopState>,
    workspace: String,
    label: String,
    branch: String,
) -> Result<(), String> {
    let (workspace, label, branch) = attach_fields(&workspace, &label, &branch)?;
    state
        .handle
        .command(SupervisorCommand::AttachWorktree {
            workspace,
            label,
            branch,
        })
        .await
        .map_err(|error| error.to_string())
}

/// The dialog may leave the name blank, but never the path or branch.
fn attach_fields(
    workspace: &str,
    label: &str,
    branch: &str,
) -> Result<(std::path::PathBuf, String, String), String> {
    let path = std::path::PathBuf::from(workspace.trim());
    let branch = branch.trim();
    if path.as_os_str().is_empty() || branch.is_empty() {
        return Err("a worktree path and its checked-out branch are required".into());
    }
    if !path.is_absolute() {
        return Err("enter the worktree's absolute path".into());
    }
    let label = match label.trim() {
        "" => path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("worktree")
            .to_string(),
        label => label.to_string(),
    };
    Ok((path, label, branch.to_string()))
}

/// Archive a managed session, then remove its worktree and its children's.
/// `discard_dirty` must only be set after the operator confirms the loss.
#[tauri::command]
pub async fn remove_worktree(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    discard_dirty: bool,
) -> Result<(), String> {
    let record = {
        let supervisor = state.supervisor.read().await;
        let supervisor = supervisor.as_ref().ok_or("Forge is shutting down")?;
        supervisor
            .session_records()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|record| record.session_id == session_id)
            .ok_or("that session no longer exists")?
    };
    if record.ownership != forge_session::WorktreeOwnership::Managed {
        return Err(
            "Only a worktree Forge created is removed with its session; close or archive this one instead"
                .into(),
        );
    }
    // A retained row is a cleanup that already failed once; archiving it again
    // would be refused, so only a live session needs the archive step.
    if record.lifecycle == SessionLifecycle::Active {
        state
            .handle
            .command(SupervisorCommand::ArchiveSession { session_id })
            .await
            .map_err(|error| error.to_string())?;
    }
    state
        .handle
        .command(SupervisorCommand::RemoveManagedWorktree {
            session_id,
            discard_dirty,
        })
        .await
        .map_err(|error| error.to_string())?;
    // A dirty worktree leaves the session retained instead of removed: say so
    // rather than reporting a cleanup that never happened.
    let lifecycle = {
        let supervisor = state.supervisor.read().await;
        let Some(supervisor) = supervisor.as_ref() else {
            return Err("Forge is shutting down".into());
        };
        supervisor
            .session_records()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|record| record.session_id == session_id)
            .map(|record| record.lifecycle)
    };
    if lifecycle == Some(SessionLifecycle::Retained) {
        return Err(
            "Its worktree still has uncommitted work. Tick “remove even if it holds uncommitted work” to discard it."
                .into(),
        );
    }
    Ok(())
}

async fn resolve(
    state: &DesktopState,
    call_id: String,
    decision: HitlDecision,
) -> Result<(), String> {
    let session_id = selected_session(state).await?;
    let pending = {
        let supervisor = state.supervisor.read().await;
        let Some(supervisor) = supervisor.as_ref() else {
            return Err("Forge is shutting down".into());
        };
        supervisor
            .snapshot(session_id)
            .await
            .and_then(|snapshot| snapshot.session.pending_hitl)
    };
    check_decision(pending.as_ref(), &call_id, &decision).map_err(|refusal| refusal.to_string())?;
    state
        .handle
        .submit(SupervisorCommand::ResolveApproval {
            session_id,
            // Always bound to the request the page showed: if it changed
            // after the check above, the session actor refuses it.
            expected_call_id: Some(call_id),
            decision,
            actor: "desktop".into(),
            feedback: None,
        })
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Approve the displayed request once. Refused for privileged requests
/// (sandbox escalation, network grant), which need the native confirmation
/// (M2); broad grants (always-allow, approve-all) have no command at all.
#[tauri::command]
pub async fn approve_once(state: State<'_, DesktopState>, call_id: String) -> Result<(), String> {
    resolve(&state, call_id, HitlDecision::Approve).await
}

#[tauri::command]
pub async fn deny(state: State<'_, DesktopState>, call_id: String) -> Result<(), String> {
    resolve(&state, call_id, HitlDecision::Deny).await
}

/// The pending request the page is acting on, by id.
async fn pending_hitl(
    state: &DesktopState,
    session_id: SessionId,
    call_id: &str,
) -> Result<HitlPayload, String> {
    let supervisor = state.supervisor.read().await;
    let supervisor = supervisor
        .as_ref()
        .ok_or_else(|| "Forge is shutting down".to_string())?;
    let snapshot = supervisor
        .snapshot(session_id)
        .await
        .ok_or_else(|| "the session is gone".to_string())?;
    snapshot
        .session
        .pending_hitl
        .filter(|payload| payload.call_id == call_id)
        .ok_or_else(|| Refusal::NotPending.to_string())
}

/// Answer with a typed grant: run once, remember the pattern, open a host,
/// trust everything for this session, refuse, or refuse with a note.
///
/// The option must be one this request offers (the backend decides that), and
/// a privileged grant needs `confirmed`, the card's second, distinct
/// confirmation that it will run outside the sandbox or open a host.
#[tauri::command]
pub async fn decide_approval(
    state: State<'_, DesktopState>,
    call_id: String,
    decision: String,
    note: Option<String>,
    confirmed: bool,
) -> Result<(), String> {
    let session_id = selected_session(&state).await?;
    let payload = pending_hitl(&state, session_id, &call_id).await?;
    let chosen = approval_options(&payload)
        .into_iter()
        .find(|option| option.decision == decision)
        .ok_or_else(|| "that decision is not offered for this request".to_string())?;
    if chosen.privileged && !confirmed {
        return Err(
            "this decision runs outside the sandbox or opens a host; confirm it first".into(),
        );
    }
    let decision = match decision.as_str() {
        "deny" => GrantDecision::Deny,
        "deny_with_note" => {
            let note = note.unwrap_or_default().trim().to_string();
            if note.is_empty() {
                return Err("a note is required for this choice".into());
            }
            GrantDecision::DenyWithNote(note)
        }
        "allow_once" => GrantDecision::AllowOnce,
        "allow_pattern_for_session" => GrantDecision::AllowPatternForSession,
        "allow_pattern_always" => GrantDecision::AllowPatternAlways,
        "allow_host_for_session" => GrantDecision::AllowHostForSession,
        "allow_host_always" => GrantDecision::AllowHostAlways,
        "approve_all" => GrantDecision::ApproveAll,
        other => return Err(format!("unknown decision `{other}`")),
    };
    state
        .handle
        .submit(SupervisorCommand::DecideApproval {
            session_id,
            expected_call_id: Some(call_id),
            decision,
            actor: "desktop".into(),
        })
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Turn approve-all on or off for the selected session. Session scoped and
/// never persisted; the sandbox still prompts for an unconfined retry.
#[tauri::command]
pub async fn set_approve_all(state: State<'_, DesktopState>, on: bool) -> Result<(), String> {
    let session_id = selected_session(&state).await?;
    state
        .handle
        .submit(SupervisorCommand::SetApproveAll { session_id, on })
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Forget every remembered pattern for the selected session.
#[tauri::command]
pub async fn clear_approvals(state: State<'_, DesktopState>) -> Result<(), String> {
    let session_id = selected_session(&state).await?;
    state
        .handle
        .submit(SupervisorCommand::ClearSessionApprovals { session_id })
        .map(drop)
        .map_err(|error| error.to_string())
}

/// One answer to one `ask_user_question` item.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AnswerInput {
    pub id: String,
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default)]
    pub custom: Option<String>,
}

/// Check the page's draft against the pending questions: every question
/// answered, every chosen label one the question offered, free-text-only
/// questions answered in words.
pub fn build_answers(
    pending: &forge_types::QuestionPayload,
    answers: &[AnswerInput],
) -> Result<Vec<AskUserQuestionAnswerItem>, String> {
    if answers.len() != pending.questions.len() {
        return Err("answer every question before sending".into());
    }
    pending
        .questions
        .iter()
        .map(|question| {
            let answer = answers
                .iter()
                .find(|answer| answer.id == question.id)
                .ok_or_else(|| format!("question `{}` is unanswered", question.id))?;
            // A label the question did not offer is dropped, not echoed back
            // as the operator's choice.
            let selected: Vec<String> = answer
                .selected
                .iter()
                .filter(|label| {
                    question
                        .options
                        .iter()
                        .any(|option| &option.label == *label)
                })
                .cloned()
                .collect();
            let custom = answer
                .custom
                .as_deref()
                .map(str::trim)
                .filter(|custom| !custom.is_empty())
                .map(str::to_string);
            if !question.multi_select && selected.len() > 1 {
                return Err(format!("question `{}` allows only one choice", question.id));
            }
            if question.options.is_empty() {
                if custom.is_none() {
                    return Err(format!("question `{}` needs an answer", question.id));
                }
            } else if selected.is_empty() && custom.is_none() {
                return Err(format!("question `{}` needs a choice", question.id));
            }
            Ok(AskUserQuestionAnswerItem {
                id: question.id.clone(),
                selected,
                custom,
            })
        })
        .collect()
}

/// Answer the pending question. `skip` dismisses it, which resolves the call
/// with a "user skipped" result rather than leaving the turn parked.
#[tauri::command]
pub async fn answer_question(
    state: State<'_, DesktopState>,
    call_id: String,
    answers: Vec<AnswerInput>,
    skip: bool,
) -> Result<(), String> {
    let session_id = selected_session(&state).await?;
    let pending = {
        let supervisor = state.supervisor.read().await;
        let supervisor = supervisor
            .as_ref()
            .ok_or_else(|| "Forge is shutting down".to_string())?;
        supervisor
            .snapshot(session_id)
            .await
            .and_then(|snapshot| snapshot.session.pending_question)
    };
    let pending = pending
        .filter(|question| question.call_id == call_id)
        .ok_or_else(|| "that question is no longer pending".to_string())?;
    let answers = if skip {
        None
    } else {
        Some(AskUserQuestionResult {
            answers: build_answers(&pending, &answers)?,
        })
    };
    state
        .handle
        .submit(SupervisorCommand::ResolveQuestion {
            session_id,
            answers,
            actor: "desktop".into(),
        })
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Resume the selected session's stopped or interrupted turn.
#[tauri::command]
pub async fn continue_turn(state: State<'_, DesktopState>) -> Result<(), String> {
    let session_id = selected_session(&state).await?;
    state
        .handle
        .submit(SupervisorCommand::ContinueTurn { session_id })
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Stop one session's turn, whether or not it is the selected one.
#[tauri::command]
pub async fn stop_session(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<(), String> {
    state
        .handle
        .submit(SupervisorCommand::StopTurn { session_id })
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Stop managing a session without archiving it; it can be resumed later.
#[tauri::command]
pub async fn close_session(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<(), String> {
    // The window always needs a selected session to resume from; closing the
    // last one would strand the durable row with no conversation to replace.
    let active = {
        let supervisor = state.supervisor.read().await;
        supervisor
            .as_ref()
            .ok_or("Forge is shutting down")?
            .snapshots()
            .await
            .into_iter()
            .filter(|snapshot| snapshot.task.lifecycle == forge_session::SessionLifecycle::Active)
            .count()
    };
    if active <= 1 {
        return Err(
            "This is the last open session; start or resume another before closing it".into(),
        );
    }
    state
        .handle
        .command(SupervisorCommand::CloseSession { session_id })
        .await
        .map_err(|error| error.to_string())
}

/// Durable sessions in the selected workspace that this process has closed.
/// They are not live rows; resuming one replaces the selected conversation.
#[tauri::command]
pub async fn list_resumable_sessions(
    state: State<'_, DesktopState>,
) -> Result<Vec<SessionRow>, String> {
    let supervisor = state.supervisor.read().await;
    let Some(supervisor) = supervisor.as_ref() else {
        return Err("Forge is shutting down".into());
    };
    let Some(selected) = supervisor
        .selected_session_id()
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(Vec::new());
    };
    let snapshots = supervisor.snapshots().await;
    let Some(active) = snapshots
        .iter()
        .find(|snapshot| snapshot.task.session_id == selected)
    else {
        return Ok(Vec::new());
    };
    let root = &active.task.workspace;
    let live: std::collections::HashSet<SessionId> = snapshots
        .iter()
        .map(|snapshot| snapshot.task.session_id)
        .collect();
    Ok(supervisor
        .session_records()
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|record| {
            !live.contains(&record.session_id)
                && record.lifecycle != forge_session::SessionLifecycle::Removed
                && same_workspace(&record.workspace, root)
        })
        .map(|record| SessionRow {
            session_id: record.session_id,
            label: escape_invisible(&record.label),
            turn_state: "idle",
            slot: record.slot,
            lifecycle: "closed",
            ownership: ownership_label(record.ownership),
        })
        .collect())
}

fn same_workspace(left: &std::path::Path, right: &std::path::Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// Fork the selected session into a new branch of the same worktree.
#[tauri::command]
pub async fn fork_session(state: State<'_, DesktopState>) -> Result<(), String> {
    let session_id = selected_session(&state).await?;
    state
        .handle
        .command(SupervisorCommand::ForkSession { session_id })
        .await
        .map_err(|error| error.to_string())
}

/// Switch to another durable session, closing nothing.
#[tauri::command]
pub async fn resume_session(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<(), String> {
    let current = selected_session(&state).await?;
    state
        .handle
        .command(SupervisorCommand::ResumeSession {
            current_session_id: current,
            session_id,
        })
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn rename_session(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    label: String,
) -> Result<(), String> {
    let label = label.trim().to_string();
    if label.is_empty() {
        return Err("session name is empty".into());
    }
    state
        .handle
        .command(SupervisorCommand::RenameSession { session_id, label })
        .await
        .map_err(|error| error.to_string())
}

/// Pin a session to a slot, or unpin it with no slot.
#[tauri::command]
pub async fn pin_session(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    slot: Option<u8>,
) -> Result<(), String> {
    state
        .handle
        .command(SupervisorCommand::PinSession {
            session_id,
            slot,
            swap: false,
        })
        .await
        .map_err(|error| error.to_string())
}

/// Remove one queued prompt, bound to the id the page showed.
#[tauri::command]
pub async fn cancel_queued(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    queue_id: u64,
    source: QueueSource,
    position: usize,
) -> Result<(), String> {
    state
        .handle
        .submit(SupervisorCommand::CancelQueuedPrompt {
            session_id,
            one_based: position,
            expected_queue_id: Some(match source {
                QueueSource::Session => {
                    forge_session::QueuedPromptId::Session(forge_types::QueueItemId(queue_id))
                }
                QueueSource::Durable => forge_session::QueuedPromptId::Durable(queue_id),
            }),
        })
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Compact the selected session's context now.
#[tauri::command]
pub async fn compact_context(state: State<'_, DesktopState>) -> Result<(), String> {
    let session_id = selected_session(&state).await?;
    state
        .handle
        .submit(SupervisorCommand::CompactContext { session_id })
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Turn extended thinking on or off for the selected session.
#[tauri::command]
pub async fn set_thinking(state: State<'_, DesktopState>, enabled: bool) -> Result<(), String> {
    let session_id = selected_session(&state).await?;
    state
        .handle
        .submit(SupervisorCommand::SetThinking {
            session_id,
            enabled,
        })
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Stop one run of a background task. Scoping by `run_id` keeps a late click
/// from cancelling a replacement run that reused the task id.
#[tauri::command]
pub async fn cancel_background(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    task_id: u64,
    run_id: String,
) -> Result<(), String> {
    let run_id: forge_core::BackgroundRunId = run_id
        .trim()
        .parse()
        .map_err(|_| "that background run is no longer valid".to_string())?;
    state
        .handle
        .submit(SupervisorCommand::CancelBackgroundRun {
            session_id,
            task_id: forge_types::BackgroundTaskId(task_id),
            run_id,
        })
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Decide a child agent's pending approval from the tasks view.
#[tauri::command]
pub async fn resolve_child_approval(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    task_id: u64,
    approve: bool,
    call_id: String,
    request_id: String,
    confirmed: bool,
) -> Result<(), String> {
    let request = {
        let supervisor = state.supervisor.read().await;
        supervisor
            .as_ref()
            .ok_or("Forge is shutting down")?
            .snapshot(session_id)
            .await
            .and_then(|snapshot| snapshot.details)
            .and_then(|details| {
                details
                    .background
                    .into_iter()
                    .find(|task| task.id.0 == task_id)
            })
            .and_then(|task| task.pending_approval)
            .filter(|request| {
                request.id.to_string() == request_id && request.payload.call_id == call_id
            })
            .ok_or("That child approval changed; inspect its current request")?
    };
    if approve && requires_native_confirm(&request.payload) && !confirmed {
        return Err("Confirm this child command's sandbox escalation or host grant first".into());
    }
    state
        .handle
        .submit(SupervisorCommand::ResolveBackgroundRequest {
            session_id,
            task_id: forge_types::BackgroundTaskId(task_id),
            request: Box::new(request),
            decision: if approve {
                HitlDecision::Approve
            } else {
                HitlDecision::Deny
            },
        })
        .map(drop)
        .map_err(|error| error.to_string())
}

/// A read-only view of one background child's saved transcript.
#[tauri::command]
pub async fn child_transcript(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    task_id: u64,
    child_session_id: SessionId,
) -> Result<Vec<MessageView>, String> {
    let journal = {
        let supervisor = state.supervisor.read().await;
        let supervisor = supervisor.as_ref().ok_or("Forge is shutting down")?;
        let child = supervisor
            .snapshot(session_id)
            .await
            .and_then(|snapshot| snapshot.details)
            .and_then(|details| {
                details
                    .background
                    .into_iter()
                    .find(|task| task.id.0 == task_id)
            })
            .and_then(|task| task.child_session_id)
            .ok_or("That background task has no child session")?;
        if child != child_session_id {
            return Err("That child session changed; reopen the task".into());
        }
        supervisor.journal_dir()
    };
    let transcript = forge_session::replayed_transcript(&journal, child_session_id, 1)
        .await
        .ok_or("could not read that child's session")?;
    Ok(transcript
        .messages()
        .iter()
        .filter_map(|message| message_view(message, true))
        .collect())
}

/// One transcript message with its full tool output and reasoning. `index`
/// counts the same filtered roles the session event sends.
#[tauri::command]
pub async fn message_detail(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    index: usize,
) -> Result<MessageView, String> {
    let supervisor = state.supervisor.read().await;
    let snapshot = supervisor
        .as_ref()
        .ok_or("Forge is shutting down")?
        .snapshot(session_id)
        .await
        .ok_or("That session is no longer live")?;
    snapshot
        .transcript
        .messages()
        .iter()
        .filter_map(|message| message_view(message, true))
        .nth(index)
        .ok_or_else(|| "that message is no longer in the transcript".to_string())
}

/// The active session's workspace root.
async fn active_workspace(state: &DesktopState) -> Result<std::path::PathBuf, String> {
    let session_id = selected_session(state).await?;
    workspace_of(state, session_id).await
}

/// One session's workspace root, by id.
async fn workspace_of(
    state: &DesktopState,
    session_id: SessionId,
) -> Result<std::path::PathBuf, String> {
    let supervisor = state.supervisor.read().await;
    let Some(supervisor) = supervisor.as_ref() else {
        return Err("Forge is shutting down".into());
    };
    supervisor
        .snapshot(session_id)
        .await
        .map(|snapshot| snapshot.session.workspace_root)
        .ok_or_else(|| "the session is gone".to_string())
}

/// The active session's workspace tree, minus runtime and dependency dirs.
#[tauri::command]
pub async fn list_files(
    state: State<'_, DesktopState>,
) -> Result<Vec<crate::workspace::FileEntry>, String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::list(&root))
        .await
        .map_err(|error| error.to_string())?
}

/// One UTF-8 file from the active session's workspace.
#[tauri::command]
pub async fn read_file(
    state: State<'_, DesktopState>,
    path: String,
) -> Result<crate::workspace::FileContents, String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::read(&root, &path))
        .await
        .map_err(|error| error.to_string())?
}

/// An image as a size-capped data URL, for the file pane and transcript chips.
#[tauri::command]
pub async fn read_image(
    state: State<'_, DesktopState>,
    path: String,
) -> Result<crate::workspace::ImageContents, String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::read_image(&root, &path))
        .await
        .map_err(|error| error.to_string())?
}

/// Branch and changed files for the active session's workspace.
#[tauri::command]
pub async fn git_status(
    state: State<'_, DesktopState>,
) -> Result<crate::workspace::WorkspaceStatus, String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::status(&root))
        .await
        .map_err(|error| error.to_string())?
}

/// The worktree diff of one file, with untracked files shown as additions.
#[tauri::command]
pub async fn git_diff(
    state: State<'_, DesktopState>,
    path: String,
    side: Option<crate::workspace::DiffSide>,
) -> Result<crate::workspace::FileDiff, String> {
    let side = side.unwrap_or_default();
    if side == crate::workspace::DiffSide::LastTurn {
        let cards = last_turn_cards(&state).await?;
        let (_, lines) = cards
            .into_iter()
            .find(|(card_path, _)| *card_path == path)
            .ok_or("that file was not written in the last turn")?;
        return Ok(crate::workspace::FileDiff {
            path,
            text: lines.join("\n"),
            truncated: false,
            side,
        });
    }
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::diff(&root, &path, side))
        .await
        .map_err(|error| error.to_string())?
}

/// The files the most recent assistant turn wrote, read from the transcript's
/// own diff cards so "last turn" stays honest after the tree moves on.
async fn last_turn_cards(state: &DesktopState) -> Result<Vec<(String, Vec<String>)>, String> {
    let session_id = selected_session(state).await?;
    let supervisor = state.supervisor.read().await;
    let snapshot = supervisor
        .as_ref()
        .ok_or("Forge is shutting down")?
        .snapshot(session_id)
        .await
        .ok_or("No live session to review")?;
    let model = forge_transcript::ConversationModel::from_messages(
        snapshot.transcript.messages(),
        snapshot.transcript.events(),
        snapshot.session.lifecycle,
        forge_transcript::ConversationViewOpts::default(),
    );
    let start = model
        .items
        .iter()
        .rposition(|item| matches!(item, forge_transcript::ChatItem::User { .. }))
        .map(|index| index + 1)
        .unwrap_or(0);
    let mut cards: Vec<(String, Vec<String>)> = Vec::new();
    for item in &model.items[start..] {
        if let forge_transcript::ChatItem::DiffCard { path, lines, .. } = item {
            match cards.iter_mut().find(|(seen, _)| seen == path) {
                Some(entry) => entry.1 = lines.clone(),
                None => cards.push((path.clone(), lines.clone())),
            }
        }
    }
    cards.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(cards)
}

/// Paths written during the most recent assistant turn.
#[tauri::command]
pub async fn git_last_turn_files(state: State<'_, DesktopState>) -> Result<Vec<String>, String> {
    Ok(last_turn_cards(&state)
        .await?
        .into_iter()
        .map(|(path, _)| path)
        .collect())
}

/// Only the fixed, typed Git verbs exposed by the shared workspace service.
#[tauri::command]
pub async fn git_operation(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    operation: crate::workspace::GitOperation,
) -> Result<String, String> {
    if selected_session(&state).await? != session_id {
        return Err("Session changed; reopen this Git action in the intended session".into());
    }
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::git_operation(&root, operation))
        .await
        .map_err(|error| error.to_string())?
}

/// Create an empty file in the active session's workspace.
#[tauri::command]
pub async fn create_file(state: State<'_, DesktopState>, path: String) -> Result<(), String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::create_file(&root, &path))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn create_dir(state: State<'_, DesktopState>, path: String) -> Result<(), String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::create_dir(&root, &path))
        .await
        .map_err(|error| error.to_string())?
}

/// Rename or move a path inside the workspace.
#[tauri::command]
pub async fn rename_path(
    state: State<'_, DesktopState>,
    from: String,
    to: String,
) -> Result<(), String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::rename(&root, &from, &to))
        .await
        .map_err(|error| error.to_string())?
}

/// Delete one file or empty directory.
#[tauri::command]
pub async fn delete_path(state: State<'_, DesktopState>, path: String) -> Result<(), String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::delete(&root, &path))
        .await
        .map_err(|error| error.to_string())?
}

/// Case-insensitive content search across the workspace.
#[tauri::command]
pub async fn search_files(
    state: State<'_, DesktopState>,
    query: String,
) -> Result<Vec<crate::workspace::SearchHit>, String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::search(&root, &query))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn git_stage(state: State<'_, DesktopState>, path: String) -> Result<(), String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::stage(&root, &path))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn git_unstage(state: State<'_, DesktopState>, path: String) -> Result<(), String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::unstage(&root, &path))
        .await
        .map_err(|error| error.to_string())?
}

/// Commit what is staged, then publish the new status on the event channel
/// through the session's own lease (the page refreshes on completion).
#[tauri::command]
pub async fn git_commit(state: State<'_, DesktopState>, message: String) -> Result<String, String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::commit(&root, &message))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn git_branches(state: State<'_, DesktopState>) -> Result<Vec<String>, String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::branches(&root))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn git_switch_branch(state: State<'_, DesktopState>, name: String) -> Result<(), String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::switch_branch(&root, &name))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn git_create_branch(state: State<'_, DesktopState>, name: String) -> Result<(), String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::create_branch(&root, &name))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn git_merge_branch(state: State<'_, DesktopState>, name: String) -> Result<(), String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::merge_branch(&root, &name))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn git_rename_branch(
    state: State<'_, DesktopState>,
    from: String,
    to: String,
) -> Result<(), String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::rename_branch(&root, &from, &to))
        .await
        .map_err(|error| error.to_string())?
}

/// One selectable model route, with the reasoning levels it accepts.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelOption {
    pub id: String,
    pub profile_id: String,
    pub provider: String,
    pub name: String,
    pub effort: Vec<String>,
    pub effort_default: String,
}

/// Runnable model routes from the connected providers' catalogs.
#[tauri::command]
pub async fn list_models() -> Result<Vec<ModelOption>, String> {
    tokio::task::spawn_blocking(|| {
        let registry = forge_connect::loaded_registry();
        let store = forge_connect::CredentialStore::user_default();
        let cache = forge_connect::ModelCatalogCache::user_default();
        forge_connect::runnable_models_for_picker(registry.profiles(), &store, &cache, false)
            .into_iter()
            .map(|entry| {
                let support = forge_connect::effort::effort_for_model(&entry.id);
                let provider = registry
                    .get(&entry.profile_id)
                    .map(|profile| {
                        if profile.provider_label.is_empty() {
                            profile.title.clone()
                        } else {
                            profile.provider_label.clone()
                        }
                    })
                    .unwrap_or_else(|| entry.profile_id.clone());
                ModelOption {
                    name: entry
                        .id
                        .split_once('/')
                        .map(|(_, model)| model.to_string())
                        .unwrap_or_else(|| entry.id.clone()),
                    id: entry.id,
                    profile_id: entry.profile_id,
                    provider,
                    effort: support
                        .options
                        .iter()
                        .map(|level| level.as_str().to_string())
                        .collect(),
                    effort_default: support.default.as_str().to_string(),
                }
            })
            .collect()
    })
    .await
    .map_err(|error| error.to_string())
}

/// Provider setup never sends a secret through the webview.
#[tauri::command]
pub async fn session_details(state: State<'_, DesktopState>) -> Result<serde_json::Value, String> {
    let id = selected_session(&state).await?;
    let details = {
        let supervisor = state.supervisor.read().await;
        supervisor
            .as_ref()
            .ok_or("Forge is shutting down")?
            .snapshot(id)
            .await
            .and_then(|snapshot| snapshot.details)
            .ok_or("Session details unavailable")?
    };
    tokio::task::spawn_blocking(move || {
        let registry = forge_connect::loaded_registry();
        let cost = match registry.get(&details.active_profile_id) {
            Some(profile) => forge_connect::provider_cost_report(profile, &details.active_model,
                details.token_usage_report.api.prompt_tokens, details.token_usage_report.api.completion_tokens,
                &forge_connect::CredentialStore::user_default()).unwrap_or_else(|error| vec![format!("Cost unavailable: {error}")]),
            None => vec!["Cost unavailable for this provider".into()],
        };
        let report = &details.token_usage_report;
        serde_json::json!({
            "tools": details.tools, "skills": details.skill_descriptions,
            "tokens": token_view(Some(&details)), "cost": cost,
            "context": {
                "System": report.system_tokens_est, "User": report.user_tokens_est,
                "Assistant": report.assistant_tokens_est, "Tools": report.tool_tokens_est,
                "Thinking": report.thinking_in_context_est, "Tool schemas": report.tool_schema_tokens_est,
            }
        })
    }).await.map_err(|error| error.to_string())
}

/// Provider setup never sends a secret through the webview.
#[tauri::command]
pub async fn list_providers() -> Result<Vec<crate::providers::ProviderView>, String> {
    tokio::task::spawn_blocking(crate::providers::list)
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn connect_provider(
    state: State<'_, DesktopState>,
    profile_id: String,
    channel: Channel<crate::providers::LoginView>,
) -> Result<String, String> {
    let message = tokio::task::spawn_blocking(move || {
        crate::providers::connect(&profile_id, |view| {
            channel.send(view).map_err(|error| error.to_string())
        })
    })
    .await
    .map_err(|error| error.to_string())??;
    state
        .handle
        .command(SupervisorCommand::ReloadProviders)
        .await
        .map_err(|error| error.to_string())?;
    Ok(message)
}

#[tauri::command]
pub async fn disconnect_provider(
    state: State<'_, DesktopState>,
    profile_id: String,
) -> Result<String, String> {
    let message = tokio::task::spawn_blocking(move || crate::providers::disconnect(&profile_id))
        .await
        .map_err(|error| error.to_string())??;
    state
        .handle
        .command(SupervisorCommand::ReloadProviders)
        .await
        .map_err(|error| error.to_string())?;
    Ok(message)
}

/// Adopt a sign-in another CLI already completed for this provider.
#[tauri::command]
pub async fn import_provider_login(
    state: State<'_, DesktopState>,
    profile_id: String,
) -> Result<String, String> {
    let message = tokio::task::spawn_blocking(move || crate::providers::import_login(&profile_id))
        .await
        .map_err(|error| error.to_string())??;
    state
        .handle
        .command(SupervisorCommand::ReloadProviders)
        .await
        .map_err(|error| error.to_string())?;
    Ok(message)
}

/// Push the linked issue branch and open its pull request.
#[tauri::command]
pub async fn github_create_pr(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    issue_number: u64,
) -> Result<(), String> {
    state
        .handle
        .command(SupervisorCommand::CreateIssuePullRequest {
            session_id,
            issue_number,
        })
        .await
        .map_err(|error| error.to_string())
}

/// Re-read the linked pull request's checks and review state.
#[tauri::command]
pub async fn github_refresh_pr(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    issue_number: u64,
) -> Result<(), String> {
    state
        .handle
        .command(SupervisorCommand::RefreshIssuePullRequest {
            session_id,
            issue_number,
        })
        .await
        .map_err(|error| error.to_string())
}

/// Queue the agent's next turn from the linked pull request's failed checks.
#[tauri::command]
pub async fn github_apply_feedback(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    issue_number: u64,
) -> Result<(), String> {
    state
        .handle
        .command(SupervisorCommand::ApplyIssuePullRequestFeedback {
            session_id,
            issue_number,
        })
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn save_custom_provider(
    state: State<'_, DesktopState>,
    id: String,
    base_url: String,
    model: String,
    wire: String,
) -> Result<String, String> {
    let message = tokio::task::spawn_blocking(move || {
        crate::providers::save_custom(&id, &base_url, &model, &wire)
    })
    .await
    .map_err(|error| error.to_string())??;
    state
        .handle
        .command(SupervisorCommand::ReloadProviders)
        .await
        .map_err(|error| error.to_string())?;
    Ok(message)
}

#[tauri::command]
pub async fn update_custom_provider(
    state: State<'_, DesktopState>,
    id: String,
    base_url: String,
    model: String,
) -> Result<String, String> {
    let message = tokio::task::spawn_blocking(move || {
        crate::providers::update_custom(&id, &base_url, &model)
    })
    .await
    .map_err(|error| error.to_string())??;
    state
        .handle
        .command(SupervisorCommand::ReloadProviders)
        .await
        .map_err(|error| error.to_string())?;
    Ok(message)
}

#[tauri::command]
pub async fn remove_custom_provider(
    state: State<'_, DesktopState>,
    id: String,
) -> Result<String, String> {
    let message = tokio::task::spawn_blocking(move || crate::providers::remove_custom(&id))
        .await
        .map_err(|error| error.to_string())??;
    state
        .handle
        .command(SupervisorCommand::ReloadProviders)
        .await
        .map_err(|error| error.to_string())?;
    Ok(message)
}

#[tauri::command]
pub async fn add_model(
    state: State<'_, DesktopState>,
    profile_id: String,
    model: String,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || forge_connect::save_manual_model(&profile_id, &model))
        .await
        .map_err(|error| error.to_string())??;
    state
        .handle
        .command(SupervisorCommand::ReloadProviders)
        .await
        .map_err(|error| error.to_string())
}

/// Switch the selected session's model (and reasoning effort).
#[tauri::command]
pub async fn set_model(
    state: State<'_, DesktopState>,
    model_id: String,
    profile_id: String,
    effort: Option<String>,
) -> Result<(), String> {
    let session_id = selected_session(&state).await?;
    let cache = forge_connect::ModelCatalogCache::user_default();
    let image_input_supported = cache.model_accepts_image_input(&model_id);
    let output = cache
        .model_limits(&model_id)
        .and_then(|limits| (limits.output > 0).then_some(limits.output));
    state
        .handle
        .command(SupervisorCommand::SetModel {
            session_id,
            model_id,
            profile_id,
            reasoning_effort: effort,
        })
        .await
        .map_err(|error| error.to_string())?;
    state
        .handle
        .command(SupervisorCommand::SetCapabilities {
            session_id,
            image_input_supported,
            context_window: Some((500_000, output)),
        })
        .await
        .map_err(|error| error.to_string())
}

/// Save an edited file back into the active session's workspace. Refused if
/// the file changed since `read_file` returned that fingerprint.
#[tauri::command]
pub async fn write_file(
    state: State<'_, DesktopState>,
    path: String,
    text: String,
    fingerprint: String,
    crlf: bool,
) -> Result<crate::workspace::FileContents, String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || {
        crate::workspace::write(&root, &path, &text, &fingerprint, crlf)
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Tell the backend whether the page holds unsaved editor buffers, so a
/// window close can ask before discarding them.
#[tauri::command]
pub fn set_unsaved(state: State<'_, DesktopState>, unsaved: bool, busy: bool) {
    state
        .unsaved
        .store(unsaved, std::sync::atomic::Ordering::Relaxed);
    state.busy.store(busy, std::sync::atomic::Ordering::Relaxed);
}

/// Quit after the user confirmed discarding unsaved buffers.
#[tauri::command]
pub fn quit_app(app: tauri::AppHandle) {
    use tauri::Manager;
    // The trust screen can quit before a session exists.
    if let Some(state) = app.try_state::<DesktopState>() {
        state
            .quitting
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    app.exit(0);
}

/// Discard the working-tree change to one tracked file.
#[tauri::command]
pub async fn github_issues(
    state: State<'_, DesktopState>,
) -> Result<Vec<serde_json::Value>, String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::github::issues(&root))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn github_details(
    state: State<'_, DesktopState>,
    issue_number: Option<u64>,
    pr_number: Option<u64>,
) -> Result<serde_json::Value, String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::github::details(&root, issue_number, pr_number))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn github_logs(
    state: State<'_, DesktopState>,
    run_id: u64,
    job_id: Option<u64>,
) -> Result<serde_json::Value, String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || {
        let logs = forge_workspace::gh::ci_logs(
            &root,
            run_id,
            job_id,
            forge_workspace::gh::MAX_CI_LOG_BYTES,
        )
        .map_err(|error| error.to_string())?;
        Ok(serde_json::json!({ "text": escape_invisible(&logs.text), "truncated": logs.truncated }))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Discard the working-tree change to one tracked file.
#[tauri::command]
pub async fn git_restore(state: State<'_, DesktopState>, path: String) -> Result<(), String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::restore(&root, &path))
        .await
        .map_err(|error| error.to_string())?
}

/// Reverse-apply one working-tree hunk. The index is the position in the
/// file's HEAD-to-worktree diff, which is exactly what the review shows.
#[tauri::command]
pub async fn git_discard_hunk(
    state: State<'_, DesktopState>,
    path: String,
    hunk_index: usize,
) -> Result<(), String> {
    let root = active_workspace(&state).await?;
    tokio::task::spawn_blocking(move || crate::workspace::discard_hunk(&root, &path, hunk_index))
        .await
        .map_err(|error| error.to_string())?
}

/// Open (or restart) the active session's interactive shell.
#[tauri::command]
pub async fn shell_open(
    state: State<'_, DesktopState>,
    channel: Channel<crate::shell::ShellEvent>,
    session_id: SessionId,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let root = workspace_of(&state, session_id).await?;
    state
        .shells
        .open(session_id, &root, cols, rows, move |event| {
            channel.send(event).is_ok()
        })
}

/// Keystrokes from the page, as UTF-8.
#[tauri::command]
pub async fn shell_write(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    data: String,
) -> Result<(), String> {
    state.shells.write(session_id, data.as_bytes())
}

#[tauri::command]
pub async fn shell_resize(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    state.shells.resize(session_id, cols, rows)
}

#[tauri::command]
pub async fn shell_close(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<(), String> {
    state.shells.close(session_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_events_pass_through_as_json() {
        let session_id = SessionId::new_v4();
        let event = translate(SupervisorEvent::Stream {
            session_id,
            event: forge_types::ModelStreamEvent::TextDelta {
                text: "hello".into(),
            },
        })
        .unwrap();
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "stream");
        assert_eq!(json["event"]["type"], "text_delta");
        assert_eq!(json["event"]["text"], "hello");
    }

    #[test]
    fn errors_and_attention_are_forwarded() {
        let session_id = SessionId::new_v4();
        assert_eq!(
            translate(SupervisorEvent::Error {
                session_id: None,
                message: "boom".into()
            }),
            Some(DesktopEvent::Error {
                message: "boom".into()
            })
        );
        let attention = translate(SupervisorEvent::Attention {
            session_id,
            state: forge_session::SupervisorTurnState::Waiting,
            message: "approve?".into(),
        })
        .unwrap();
        assert_eq!(
            serde_json::to_value(&attention).unwrap()["turn_state"],
            forge_session::SupervisorTurnState::Waiting.label()
        );
    }

    #[test]
    fn selection_is_forwarded_without_its_transcript() {
        let session_id = SessionId::new_v4();
        assert_eq!(
            translate(SupervisorEvent::Selected(Some(session_id))),
            Some(DesktopEvent::Selected {
                session_id: Some(session_id)
            })
        );
        assert_eq!(
            translate(SupervisorEvent::Selected(None)),
            Some(DesktopEvent::Selected { session_id: None })
        );
    }

    fn snapshot(
        id: SessionId,
        label: &str,
        lifecycle: SessionLifecycle,
        turn_state: forge_session::SupervisorTurnState,
    ) -> SessionRuntimeSnapshot {
        SessionRuntimeSnapshot {
            task: RepositorySession {
                session_id: id,
                label: label.into(),
                workspace: std::path::PathBuf::new(),
                branch: String::new(),
                base_sha: None,
                ownership: forge_session::WorktreeOwnership::Primary,
                lifecycle,
                turn_state,
                slot: None,
                model_id: String::new(),
                profile_id: String::new(),
                reasoning_effort: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                archived_at: None,
                github_issue_number: None,
                github_pr_url: None,
                github_auto_pr: false,
                github_retry_attempts: 0,
                github_tests_passed: false,
            },
            session: forge_session::SessionSnapshot::default(),
            transcript: forge_session::TranscriptSnapshot::default(),
            details: None,
            queued_prompts: Vec::new(),
            interrupted_prompts: Vec::new(),
        }
    }

    /// Every session the supervisor knows reaches the switcher, with its
    /// lifecycle so the page can separate live rows from resumable ones.
    #[test]
    fn the_roster_lists_live_sessions() {
        use forge_session::SupervisorTurnState;
        let live = SessionId::new_v4();
        let archived = SessionId::new_v4();
        let event = translate(SupervisorEvent::Roster(vec![
            snapshot(
                live,
                "fix the parser",
                SessionLifecycle::Active,
                SupervisorTurnState::Running,
            ),
            snapshot(
                archived,
                "old work",
                SessionLifecycle::Archived,
                SupervisorTurnState::Idle,
            ),
        ]))
        .unwrap();
        assert_eq!(
            event,
            DesktopEvent::Roster {
                sessions: vec![
                    SessionRow {
                        session_id: live,
                        label: "fix the parser".into(),
                        turn_state: "running",
                        slot: None,
                        lifecycle: "active",
                        ownership: "primary",
                    },
                    SessionRow {
                        session_id: archived,
                        label: "old work".into(),
                        turn_state: "idle",
                        slot: None,
                        lifecycle: "archived",
                        ownership: "primary",
                    },
                ]
            }
        );
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "roster");
        assert_eq!(json["sessions"][0]["session_id"], live.to_string());
        assert!(json["sessions"][0].get("workspace").is_none());
    }

    #[test]
    fn durable_queue_rows_keep_their_identity_without_actor_details() {
        let mut snapshot = snapshot(
            SessionId::new_v4(),
            "queue",
            SessionLifecycle::Active,
            forge_session::SupervisorTurnState::Idle,
        );
        snapshot.queued_prompts = vec![(42, "next prompt".into())];
        let DesktopEvent::Session { queue, queued, .. } = session_event(&snapshot) else {
            panic!("expected session")
        };
        assert_eq!(queued, 1);
        assert_eq!(
            queue,
            vec![QueueView {
                id: 42,
                source: QueueSource::Durable,
                text: "next prompt".into()
            }]
        );
    }

    fn payload(sandbox_escalation: bool, denied_host: Option<&str>) -> HitlPayload {
        HitlPayload {
            call_id: "call-1".into(),
            tool: "bash".into(),
            args_redacted: serde_json::json!({"command": "curl example.com"}),
            reason: "needs approval".into(),
            failure: None,
            sandbox_escalation,
            denied_host: denied_host.map(str::to_string),
        }
    }

    #[test]
    fn privileged_requests_can_only_be_denied_from_the_page() {
        for privileged in [payload(true, None), payload(false, Some("example.com"))] {
            assert!(requires_native_confirm(&privileged));
            assert_eq!(
                check_decision(Some(&privileged), "call-1", &HitlDecision::Approve),
                Err(Refusal::NeedsNativeConfirm),
                "{privileged:?}"
            );
            assert_eq!(
                check_decision(Some(&privileged), "call-1", &HitlDecision::Deny),
                Ok(())
            );
        }
    }

    #[test]
    fn a_plain_request_approves_once() {
        let plain = payload(false, None);
        assert!(!requires_native_confirm(&plain));
        assert_eq!(
            check_decision(Some(&plain), "call-1", &HitlDecision::Approve),
            Ok(())
        );
    }

    #[test]
    fn decisions_need_the_request_that_is_actually_pending() {
        let plain = payload(false, None);
        for decision in [HitlDecision::Approve, HitlDecision::Deny] {
            assert_eq!(
                check_decision(None, "call-1", &decision),
                Err(Refusal::NotPending)
            );
            assert_eq!(
                check_decision(Some(&plain), "call-2", &decision),
                Err(Refusal::NotPending)
            );
            assert_eq!(
                check_decision(Some(&plain), " ", &decision),
                Err(Refusal::NoApprovalId)
            );
        }
    }

    fn question_payload() -> forge_types::QuestionPayload {
        use forge_types::{AskUserQuestionItem, AskUserQuestionOption};
        forge_types::QuestionPayload {
            call_id: "ask-1".into(),
            tool: "ask_user_question".into(),
            questions: vec![
                AskUserQuestionItem {
                    id: "q1".into(),
                    header: "Approach".into(),
                    question: "Which approach?".into(),
                    options: vec![
                        AskUserQuestionOption {
                            label: "Fast".into(),
                            description: "Ship it".into(),
                        },
                        AskUserQuestionOption {
                            label: "Safe".into(),
                            description: "Test it".into(),
                        },
                    ],
                    multi_select: false,
                },
                AskUserQuestionItem {
                    id: "q2".into(),
                    header: "Notes".into(),
                    question: "Anything else?".into(),
                    options: vec![],
                    multi_select: false,
                },
            ],
        }
    }

    #[test]
    fn answers_must_cover_every_question_and_offer_only_known_labels() {
        let pending = question_payload();
        let answers = [
            AnswerInput {
                id: "q1".into(),
                selected: vec!["Fast".into(), "Not offered".into()],
                custom: None,
            },
            AnswerInput {
                id: "q2".into(),
                selected: vec![],
                custom: Some("  careful  ".into()),
            },
        ];
        let built = build_answers(&pending, &answers).unwrap();
        assert_eq!(built[0].selected, vec!["Fast".to_string()]);
        assert_eq!(built[1].custom.as_deref(), Some("careful"));

        // A free-text-only question cannot be answered with a label.
        let missing = [
            AnswerInput {
                id: "q1".into(),
                selected: vec!["Safe".into()],
                custom: None,
            },
            AnswerInput {
                id: "q2".into(),
                selected: vec![],
                custom: Some("   ".into()),
            },
        ];
        assert!(build_answers(&pending, &missing).is_err());

        // A choice question with nothing chosen and no words is unanswered.
        let empty = [
            AnswerInput {
                id: "q1".into(),
                selected: vec![],
                custom: None,
            },
            AnswerInput {
                id: "q2".into(),
                selected: vec![],
                custom: Some("x".into()),
            },
        ];
        assert!(build_answers(&pending, &empty).is_err());
        assert!(build_answers(&pending, &[]).is_err());
        let mut multiple = answers.clone();
        multiple[0].selected = vec!["Fast".into(), "Safe".into()];
        assert!(build_answers(&pending, &multiple).is_err());
        let mut raw_label = pending.clone();
        raw_label.questions[0].options[0].label = "Fast\u{202e}".into();
        let shown = question_view(&raw_label);
        assert_eq!(shown.questions[0].options[0].value, "Fast\u{202e}");
        assert!(!shown.questions[0].options[0].label.contains('\u{202e}'));
    }

    #[test]
    fn grant_options_match_the_kind_of_request() {
        let governance = approval_options(&payload(false, None));
        let names: Vec<&str> = governance
            .iter()
            .map(|option| option.decision.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "allow_once",
                "allow_pattern_for_session",
                "allow_pattern_always",
                "approve_all",
                "deny",
                "deny_with_note"
            ]
        );
        assert!(governance.iter().all(|option| !option.privileged));

        let sandbox = approval_options(&payload(true, None));
        assert!(!sandbox
            .iter()
            .any(|option| option.decision == "approve_all"));
        assert!(sandbox.iter().any(|option| option.privileged));

        let host = approval_options(&payload(false, Some("example.com")));
        assert!(host
            .iter()
            .any(|option| option.decision == "allow_host_for_session"
                && option.label.contains("example.com")));
        assert!(host.iter().filter(|option| option.privileged).count() >= 2);
        assert!(host.iter().all(|option| option.decision != "allow_once"));
    }

    /// The page trusts a workspace before it subscribes, so the answer has to
    /// flip exactly when the deferred launch is consumed.
    #[test]
    fn the_trust_screen_lasts_until_the_deferred_launch_is_taken() {
        let ready = TrustState::ready(std::path::PathBuf::from("/work/repo"));
        assert_eq!(trust_view(&ready).workspace, "/work/repo");
        assert!(!trust_view(&ready).needs_trust);

        let pending = TrustState::pending(
            std::path::PathBuf::from("/work/repo"),
            forge_config::Config::default(),
        );
        assert!(trust_view(&pending).needs_trust);
        assert!(pending.deferred.lock().unwrap().take().is_some());
        assert!(!trust_view(&pending).needs_trust);
        assert_eq!(pending.fail("cannot start".into()), "cannot start");
        assert_eq!(trust_view(&pending).error, "cannot start");
    }

    #[test]
    fn attaching_a_worktree_requires_a_path_branch_and_absolute_path() {
        assert!(attach_fields("", "name", "main").is_err());
        assert!(attach_fields("/work/linked", "name", " ").is_err());
        assert!(attach_fields("relative/linked", "name", "main").is_err());
        let (path, label, branch) =
            attach_fields("/work/linked", " ", "feature/x").expect("valid attach");
        assert_eq!(path, std::path::PathBuf::from("/work/linked"));
        assert_eq!(label, "linked");
        assert_eq!(branch, "feature/x");
    }

    #[test]
    fn invisible_and_bidi_characters_are_made_visible() {
        // "rm -rf ~ #" disguised with a right-to-left override and a
        // zero-width space.
        let disguised = "echo safe\u{202E}fr- mr\u{200B}";
        let shown = escape_invisible(disguised);
        assert_eq!(shown, "echo safe⟨U+202E⟩fr- mr⟨U+200B⟩");
        assert!(!shown.chars().any(|c| matches!(c, '\u{202E}' | '\u{200B}')));
        assert_eq!(escape_invisible("plain ascii"), "plain ascii");
        assert_eq!(
            escape_invisible("\u{2066}x\u{2069}\u{FEFF}"),
            "⟨U+2066⟩x⟨U+2069⟩⟨U+FEFF⟩"
        );
    }
}
