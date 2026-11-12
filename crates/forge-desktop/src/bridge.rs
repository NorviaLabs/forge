//! The IPC bridge: Forge's supervisor on one side, the webview on the other.
//!
//! Every open project goes through one `ProjectService`. Commands that name
//! a session act on that session's project; commands on the current
//! selection, and project-level ones, act on the project on screen. The
//! page receives the on-screen project's events, plus a `Workspace` summary
//! of every project and `AttentionNew` when requests arrive. This is the
//! desktop's permanent transport (doc 2, D8); there is no protocol rewrite
//! planned behind it.

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::State;

use forge_session::{
    RepositorySession, SessionLifecycle, SessionRuntimeSnapshot, SupervisorCommand, SupervisorEvent,
};
use forge_types::{
    ApprovalKind, AskUserQuestionAnswerItem, AskUserQuestionResult, GrantDecision, HitlDecision,
    HitlPayload, MessageRole, ModelStreamEvent, SessionId,
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
        /// A wait with no card of its own (a typed reply, a missing setting,
        /// something outside Forge), so the page can say what to do.
        waiting_on: Option<WaitingView>,
        /// Per-session approve-all: review prompts auto-approved, the sandbox
        /// still enforced.
        approve_all: bool,
        /// Hosts a running command is waiting to reach. Answered with
        /// `answer_host_question` while the turn is still running.
        host_questions: Vec<HostQuestionView>,
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
        skill_descriptions: Vec<(String, String)>,
        pattern_allows: usize,
        image_input: bool,
        /// Whether the switch for extended thinking changes anything for
        /// this session's model, so the page only offers it where it works.
        thinking_switchable: bool,
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
    /// A session not on screen changed turn state. Sent instead of its full
    /// `Session` snapshot, which the page would only discard.
    SessionState {
        session_id: SessionId,
        turn_state: &'static str,
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
    /// Every registered project, and which one is on screen. Sent whenever a
    /// project opens, closes, or its sessions or pending requests change.
    Workspace {
        active: forge_session::ProjectId,
        projects: Vec<ProjectView>,
    },
    /// Requests arrived that weren't pending before. `sound` is false when
    /// all of them are in the session on screen and the window is focused.
    AttentionNew {
        count: usize,
        sound: bool,
    },
    /// A project that isn't on screen reported an error or a failed turn.
    /// The on-screen project's errors arrive as `Error` and `Attention`.
    ProjectError {
        project_id: forge_session::ProjectId,
        project_name: String,
        message: String,
    },
    /// Another project is now on screen. Its roster, selection and selected
    /// session follow as ordinary events.
    ProjectSwitched {
        project_id: forge_session::ProjectId,
        supports_git: bool,
    },
}

/// One project in the switcher.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProjectView {
    pub id: forge_session::ProjectId,
    pub name: String,
    pub root: String,
    pub open: bool,
    pub running: usize,
    pub limit: usize,
    /// Sessions queued, running, or waiting on you: what quitting would stop.
    pub busy: usize,
    /// Requests waiting on you, oldest first.
    pub attention: Vec<AttentionView>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AttentionView {
    pub session_id: SessionId,
    pub session_label: String,
    /// `approval`, `question` or `child_approval`.
    pub kind: &'static str,
    pub call_id: String,
    pub since: String,
}

/// Errors from projects not on screen that `seen` hasn't shown yet. Updates
/// `seen`, including for the on-screen project, so switching away doesn't
/// replay an error the page already showed.
pub fn fresh_project_errors(
    seen: &mut std::collections::HashMap<forge_session::ProjectId, u64>,
    workspace: &forge_session::WorkspaceState,
    active: forge_session::ProjectId,
) -> Vec<DesktopEvent> {
    let mut fresh = Vec::new();
    for summary in &workspace.projects {
        let Some(notice) = &summary.last_error else {
            continue;
        };
        let previous = seen.insert(summary.project.id, notice.seq);
        if summary.project.id != active && previous.is_none_or(|seq| seq < notice.seq) {
            fresh.push(DesktopEvent::ProjectError {
                project_id: summary.project.id,
                project_name: summary.project.name.clone(),
                message: notice.message.clone(),
            });
        }
    }
    fresh
}

pub fn project_views(workspace: &forge_session::WorkspaceState) -> Vec<ProjectView> {
    workspace
        .projects
        .iter()
        .map(|summary| ProjectView {
            id: summary.project.id,
            name: summary.project.name.clone(),
            root: summary.project.root.display().to_string(),
            open: summary.open,
            running: summary.state.running,
            limit: summary.state.limit,
            busy: summary
                .state
                .sessions
                .iter()
                .filter(|session| {
                    matches!(
                        session.turn_state,
                        forge_session::SupervisorTurnState::Queued
                            | forge_session::SupervisorTurnState::Running
                            | forge_session::SupervisorTurnState::Waiting
                    )
                })
                .count(),
            attention: summary
                .state
                .attention
                .iter()
                .map(|item| AttentionView {
                    session_id: item.session_id,
                    session_label: summary
                        .state
                        .sessions
                        .iter()
                        .find(|session| session.session_id == item.session_id)
                        .map(|session| session.label.clone())
                        .unwrap_or_default(),
                    kind: match item.kind {
                        forge_session::AttentionKind::Approval => "approval",
                        forge_session::AttentionKind::Question => "question",
                        forge_session::AttentionKind::ChildApproval => "child_approval",
                    },
                    call_id: item.call_id.clone(),
                    since: item.since.to_rfc3339(),
                })
                .collect(),
        })
        .collect()
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
    /// `primary`, `managed`, `attached` or `directory`. Only a managed
    /// worktree can be removed with the session; an attached one belongs to
    /// the operator, and a directory session runs in the project folder.
    pub ownership: &'static str,
}

fn ownership_label(ownership: forge_session::WorktreeOwnership) -> &'static str {
    match ownership {
        forge_session::WorktreeOwnership::Primary => "primary",
        forge_session::WorktreeOwnership::Managed => "managed",
        forge_session::WorktreeOwnership::Attached => "attached",
        forge_session::WorktreeOwnership::Directory => "directory",
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
                "Approves review prompts until the session ends without asking. The sandbox stays on: what needs to leave it fails.",
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
///
/// An assistant message with nothing to show (a reply that is only tool
/// calls) has no view: its tool results follow as their own rows, and an
/// empty one would leave a bare "Forge" heading above them. Whether a
/// message has a view never depends on `full`, so indexes agree between the
/// session event and the detail command.
fn message_view(message: &forge_types::Message, full: bool) -> Option<MessageView> {
    let role = match message.role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
        _ => return None,
    };
    // The failure marker tells the model (and resume) that a turn failed.
    // It is not prose: the operator reads the reason that follows it.
    let content = match message.role {
        MessageRole::Assistant => message
            .content
            .strip_prefix(forge_core::TURN_FAILED_MARKER)
            .unwrap_or(&message.content),
        _ => &message.content,
    };
    if message.role == MessageRole::Assistant
        && content.trim().is_empty()
        && message
            .thinking
            .as_deref()
            .is_none_or(|text| text.trim().is_empty())
        && message.attachments.is_empty()
    {
        return None;
    }
    let text = if !full && message.role == MessageRole::Tool {
        truncate(content, TOOL_TEXT_LIMIT)
    } else {
        content.to_string()
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

/// What a waiting session needs from the operator when no card shows it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WaitingView {
    /// `reply`, `configuration`, `external` or `other`.
    pub kind: &'static str,
    /// The missing key, or the outside action, in the agent's words.
    pub detail: String,
}

fn waiting_view(waiting: &forge_session::WaitingOn) -> WaitingView {
    use forge_session::WaitingOn;
    match waiting {
        WaitingOn::Reply => WaitingView {
            kind: "reply",
            detail: String::new(),
        },
        WaitingOn::Configuration(key) => WaitingView {
            kind: "configuration",
            detail: escape_invisible(key),
        },
        WaitingOn::ExternalAction(description) => WaitingView {
            kind: "external",
            detail: escape_invisible(description),
        },
        WaitingOn::Other => WaitingView {
            kind: "other",
            detail: String::new(),
        },
    }
}

/// Whether turning thinking off reaches the model. Anthropic reasoning
/// models take an explicit "disabled"; the other wires only honour it when
/// the catalog lists "none" among the model's reasoning levels. Asked once
/// per model: the answer reads the provider registry and the catalog file.
fn thinking_switchable(model: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static KNOWN: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    if model.is_empty() {
        return false;
    }
    let known = KNOWN.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(answer) = known.lock().ok().and_then(|map| map.get(model).copied()) {
        return answer;
    }
    let answer = forge_connect::effort::effort_for_model(model).supported()
        || forge_connect::ModelCatalogCache::user_default().model_supports_thinking_off(model);
    if let Ok(mut map) = known.lock() {
        map.insert(model.to_string(), answer);
    }
    answer
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
        waiting_on: snapshot.session.waiting_on.as_ref().map(waiting_view),
        approve_all: snapshot.session.approve_all,
        host_questions: snapshot
            .host_questions
            .iter()
            .map(|question| HostQuestionView {
                host: question.host.clone(),
                port: question.port,
                call_id: question.call_id.clone(),
                write: question.write.clone(),
            })
            .collect(),
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
        skill_descriptions: snapshot
            .details
            .as_ref()
            .map(|details| {
                details
                    .skill_descriptions
                    .iter()
                    .map(|(name, description)| {
                        (escape_invisible(name), escape_invisible(description))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        pattern_allows: snapshot
            .details
            .as_ref()
            .map(|details| details.session_pattern_allow_count)
            .unwrap_or_default(),
        image_input: snapshot
            .details
            .as_ref()
            .is_some_and(|details| details.image_input_supported),
        thinking_switchable: snapshot
            .details
            .as_ref()
            .is_some_and(|details| thinking_switchable(&details.active_model)),
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
/// Most text deltas one coalesced `Stream` event carries.
const MAX_COALESCED_DELTA: usize = 16 * 1024;

/// What the page needs from a supervisor event, given the session on screen.
/// Streamed output and full snapshots of other sessions are never sent: the
/// page discards them, and building a snapshot event copies the whole
/// transcript. Only text deltas are streamed; the page ignores other kinds.
pub fn route(event: SupervisorEvent, on_screen: Option<SessionId>) -> Option<DesktopEvent> {
    match event {
        SupervisorEvent::Stream { session_id, event } => {
            let ModelStreamEvent::TextDelta { .. } = &event else {
                return None;
            };
            (Some(session_id) == on_screen)
                .then(|| translate(SupervisorEvent::Stream { session_id, event }))
                .flatten()
        }
        SupervisorEvent::SessionUpdated(snapshot)
            if Some(snapshot.task.session_id) != on_screen =>
        {
            Some(DesktopEvent::SessionState {
                session_id: snapshot.task.session_id,
                turn_state: snapshot.task.turn_state.label(),
            })
        }
        event => translate(event),
    }
}

/// The text of a delta for `session_id`, or `None` for anything else.
fn delta_text(event: &SupervisorEvent, session_id: SessionId) -> Option<&str> {
    match event {
        SupervisorEvent::Stream {
            session_id: id,
            event: ModelStreamEvent::TextDelta { text },
        } if *id == session_id => Some(text),
        _ => None,
    }
}

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
    pub projects: forge_session::ProjectServiceHandle,
    /// The project on screen. Commands that act on the current selection
    /// (approve, answer, stop, model) and project-level commands (create,
    /// attach) use it; commands that name a session use that session's
    /// project instead, so a switch can never misroute them.
    active: tokio::sync::watch::Sender<forge_session::OpenProjectRef>,
    /// The session's interactive shells (one per session, opened on demand).
    pub shells: crate::shell::Shells,
    /// One watched search index per workspace root, opened on the first
    /// search there. Sessions in managed worktrees each have their own root.
    indexes: crate::workspace::Indexes,
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
        projects: forge_session::ProjectServiceHandle,
        active: forge_session::OpenProjectRef,
    ) -> Self {
        Self {
            projects,
            active: tokio::sync::watch::channel(active).0,
            shells: crate::shell::Shells::default(),
            indexes: crate::workspace::Indexes::default(),
            unsaved: std::sync::atomic::AtomicBool::new(false),
            busy: std::sync::atomic::AtomicBool::new(false),
            quitting: std::sync::atomic::AtomicBool::new(false),
            #[cfg(feature = "self-test")]
            self_test: None,
        }
    }

    /// The project on screen.
    pub fn active(&self) -> forge_session::OpenProjectRef {
        self.active.borrow().clone()
    }

    /// The open project that owns `session_id`, live or closed. A session no
    /// open project knows is an error, never the project on screen.
    pub async fn project_for(
        &self,
        session_id: SessionId,
    ) -> Result<forge_session::OpenProjectRef, SessionFailure> {
        Ok(self.projects.project_of(session_id).await?)
    }

    /// Put another open project on screen.
    pub fn switch_to(&self, project: forge_session::OpenProjectRef) {
        self.active.send_replace(project);
    }

    /// Provider credentials and definitions are user-level: every open
    /// project picks the change up.
    async fn reload_providers(&self) -> Result<(), String> {
        let open = self
            .projects
            .open_projects()
            .await
            .map_err(|error| error.to_string())?;
        for project in open {
            project
                .handle
                .command(SupervisorCommand::ReloadProviders)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

/// What the trust screen is waiting for the operator to accept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    /// A registered project that isn't trusted.
    Trust(forge_session::ProjectId),
    /// A plain folder no project contains: accepting registers it, trusts
    /// it, and opens it.
    Confirm(std::path::PathBuf),
}

/// Whether the window should show the trust screen. Present in every build:
/// the page asks before it subscribes.
pub struct TrustState {
    projects: forge_session::ProjectServiceHandle,
    /// `None` until the operator picks a folder: a launch with no path from
    /// `/` or the home folder never opens either implicitly.
    workspace: std::sync::Mutex<Option<std::path::PathBuf>>,
    /// `Some` until the operator accepts and the project opens.
    pending: std::sync::Mutex<Option<Pending>>,
    /// Why opening after trust failed, so a retry can say what happened.
    error: std::sync::Mutex<String>,
}

impl TrustState {
    pub fn ready(
        projects: forge_session::ProjectServiceHandle,
        workspace: std::path::PathBuf,
    ) -> Self {
        Self {
            projects,
            workspace: std::sync::Mutex::new(Some(workspace)),
            pending: std::sync::Mutex::new(None),
            error: std::sync::Mutex::new(String::new()),
        }
    }

    pub fn pending(
        projects: forge_session::ProjectServiceHandle,
        workspace: std::path::PathBuf,
        pending: Pending,
    ) -> Self {
        let state = Self::ready(projects, workspace);
        state.restore(pending);
        state
    }

    /// Nothing open yet: the page shows a folder picker.
    pub fn needs_folder(projects: forge_session::ProjectServiceHandle) -> Self {
        Self {
            projects,
            workspace: std::sync::Mutex::new(None),
            pending: std::sync::Mutex::new(None),
            error: std::sync::Mutex::new(String::new()),
        }
    }

    fn workspace(&self) -> Option<std::path::PathBuf> {
        self.workspace.lock().ok().and_then(|slot| slot.clone())
    }

    fn set_workspace(&self, workspace: std::path::PathBuf) {
        if let Ok(mut slot) = self.workspace.lock() {
            *slot = Some(workspace);
        }
    }

    fn restore(&self, pending: Pending) {
        if let Ok(mut slot) = self.pending.lock() {
            *slot = Some(pending);
        }
    }

    fn take_pending(&self) -> Option<Pending> {
        self.pending.lock().ok().and_then(|mut slot| slot.take())
    }

    /// No folder yet, or one that is only being asked about. Once a folder
    /// has opened (a workspace with nothing pending) the picker is closed.
    fn can_choose_folder(&self) -> bool {
        self.workspace().is_none() || self.pending.lock().is_ok_and(|slot| slot.is_some())
    }

    /// Drop the folder the trust screen was asking about, and its error,
    /// before another one takes its place. Nothing was registered for a
    /// folder that was only being confirmed.
    fn forget_pending(&self) {
        self.take_pending();
        if let Ok(mut slot) = self.workspace.lock() {
            *slot = None;
        }
        if let Ok(mut error) = self.error.lock() {
            error.clear();
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
    /// No folder is open yet; the page shows the folder picker.
    pub needs_folder: bool,
    pub needs_trust: bool,
    /// The folder isn't a project yet; accepting adds it.
    pub needs_confirmation: bool,
    /// The folder awaiting trust isn't a Git repository, so the trust screen
    /// warns that agent edits can't be rolled back.
    pub no_git: bool,
    pub error: String,
}

/// The page asks this first. `needs_trust` means: show the trust screen and
/// do not subscribe yet.
#[tauri::command]
pub fn trust_status(state: State<'_, TrustState>) -> TrustView {
    trust_view(&state)
}

pub fn trust_view(state: &TrustState) -> TrustView {
    let workspace = state.workspace();
    let pending = state.pending.lock().ok().and_then(|slot| slot.clone());
    // Only asked while the trust screen shows: detection runs `git`.
    let no_git = pending.is_some()
        && workspace.as_deref().is_some_and(|workspace| {
            forge_session::ProjectTopology::detect(workspace)
                .is_ok_and(|topology| !topology.supports_git())
        });
    TrustView {
        needs_folder: workspace.is_none(),
        workspace: workspace
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        needs_trust: pending.is_some(),
        needs_confirmation: matches!(pending, Some(Pending::Confirm(_))),
        no_git,
        error: state
            .error
            .lock()
            .map(|error| error.clone())
            .unwrap_or_default(),
    }
}

/// Accept what the trust screen asked: register the folder if it is new,
/// record trust, then open it in this window.
///
/// Trust is written to the same store `forge` reads, so the terminal and the
/// desktop agree from here on. Opening re-reads trust from disk, which is the
/// check that would catch a store that refused the write.
#[tauri::command]
pub async fn trust_workspace(
    app: tauri::AppHandle,
    state: State<'_, TrustState>,
) -> Result<(), String> {
    let Some(pending) = state.take_pending() else {
        return Err("this workspace is already trusted".into());
    };
    let result = async {
        let id = match &pending {
            Pending::Trust(id) => *id,
            Pending::Confirm(root) => state.projects.add(root.clone()).await?.id,
        };
        state.projects.trust(id).await?;
        open_in_window(&app, &state.projects, id).await
    }
    .await;
    if let Err(error) = result {
        state.restore(pending);
        return Err(state.fail(error.to_string()));
    }
    if let Ok(mut error) = state.error.lock() {
        error.clear();
    }
    Ok(())
}

/// Ask for the folder to open, for a launch that had none. Picking a folder
/// is the confirmation that it is a project; one that still needs trust goes
/// to the trust screen, a trusted one opens now. Cancelling keeps the picker.
#[tauri::command]
pub async fn choose_folder(
    app: tauri::AppHandle,
    state: State<'_, TrustState>,
) -> Result<TrustView, String> {
    use tauri::Manager;
    // A folder still waiting on the trust screen is not open: the operator
    // may pick a different one instead (a wrong pick used to mean quitting).
    if !state.can_choose_folder() || app.try_state::<DesktopState>().is_some() {
        return Err("a folder is already open".into());
    }
    let Some(folder) = pick_folder(&app, "Open a folder in Forge")
        .await
        .map_err(|error| state.fail(error))?
    else {
        // Cancelled: whatever was on screen stays.
        return Ok(trust_view(&state));
    };
    state.forget_pending();
    match picked_folder(&state.projects, folder, forge_config::is_trusted).await {
        Ok(Picked::Project(project)) => open_or_ask_trust(&app, &state, project).await,
        Ok(Picked::AskFirst(root)) => {
            state.set_workspace(root.clone());
            state.restore(Pending::Confirm(root));
            Ok(trust_view(&state))
        }
        Err(error) => Err(state.fail(error)),
    }
}

/// What a folder pick leads to.
#[derive(Debug)]
enum Picked {
    /// A registered project: open it, or ask to trust it.
    Project(forge_session::Project),
    /// A new, untrusted folder: ask first, registering nothing yet.
    AskFirst(std::path::PathBuf),
}

/// Resolve a picked folder. A folder inside a registered project is that
/// project. A new folder that is already trusted is registered and opens
/// now. A new one that is not goes to the trust screen unregistered, as a
/// launch from it would: registering first left a mistaken pick (the home
/// folder, say) in the registry after the operator backed out, where it
/// captured every later launch from beneath it.
async fn picked_folder(
    projects: &forge_session::ProjectServiceHandle,
    folder: std::path::PathBuf,
    is_trusted: impl Fn(&std::path::Path) -> bool,
) -> Result<Picked, String> {
    match projects
        .resolve_launch(folder)
        .await
        .map_err(|error| error.to_string())?
    {
        forge_session::LaunchResolution::Project(project) => Ok(Picked::Project(project)),
        forge_session::LaunchResolution::NeedsConfirmation { root } if !is_trusted(&root) => {
            Ok(Picked::AskFirst(root))
        }
        forge_session::LaunchResolution::NeedsConfirmation { root } => projects
            .add(root)
            .await
            .map(Picked::Project)
            .map_err(|error| error.to_string()),
    }
}

/// Registered projects, for the folder screen before any project is open.
#[tauri::command]
pub fn list_projects(state: State<'_, TrustState>) -> Vec<ProjectView> {
    project_views(&state.projects.workspace().borrow())
}

/// Open a registered project from the folder screen: the last project on a
/// Dock launch, or one the operator picks from the list.
#[tauri::command]
pub async fn open_project(
    app: tauri::AppHandle,
    state: State<'_, TrustState>,
    project_id: forge_session::ProjectId,
) -> Result<TrustView, String> {
    use tauri::Manager;
    if state.workspace().is_some() || app.try_state::<DesktopState>().is_some() {
        return Err("a folder is already open".into());
    }
    let project = state
        .projects
        .workspace()
        .borrow()
        .projects
        .iter()
        .find(|summary| summary.project.id == project_id)
        .map(|summary| summary.project.clone());
    let Some(project) = project else {
        return Err(state.fail("that project is no longer registered".into()));
    };
    open_or_ask_trust(&app, &state, project).await
}

/// Open `project` in the window, or leave it on the trust screen when it
/// isn't trusted. Any other failure keeps the folder screen, with the reason.
async fn open_or_ask_trust(
    app: &tauri::AppHandle,
    state: &TrustState,
    project: forge_session::Project,
) -> Result<TrustView, String> {
    match open_in_window(app, &state.projects, project.id).await {
        Ok(()) => state.set_workspace(project.root.clone()),
        Err(forge_session::ProjectError::NeedsTrust(_)) => {
            state.set_workspace(project.root.clone());
            state.restore(Pending::Trust(project.id));
        }
        Err(error) => return Err(state.fail(error.to_string())),
    }
    Ok(trust_view(state))
}

/// A native folder picker, created on the main thread as macOS requires.
/// `None` when the operator cancels.
pub(crate) async fn pick_folder(
    app: &tauri::AppHandle,
    title: &'static str,
) -> Result<Option<std::path::PathBuf>, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = sender.send(rfd::AsyncFileDialog::new().set_title(title).pick_folder());
    })
    .map_err(|error| error.to_string())?;
    let pick = receiver
        .await
        .map_err(|_| "the folder picker did not open".to_string())?;
    Ok(pick.await.map(|picked| picked.path().to_path_buf()))
}

/// Open a project and make it the window's first project.
async fn open_in_window(
    app: &tauri::AppHandle,
    projects: &forge_session::ProjectServiceHandle,
    id: forge_session::ProjectId,
) -> Result<(), forge_session::ProjectError> {
    use tauri::Manager;
    projects.open(id).await?;
    let project = projects.project(id).await?;
    app.manage(DesktopState::new(projects.clone(), project));
    Ok(())
}

/// What `subscribe` returns before events start flowing.
#[derive(Debug, Serialize)]
pub struct Hello {
    pub project_id: forge_session::ProjectId,
    pub session_id: SessionId,
    pub initial: DesktopEvent,
    pub sessions: Vec<SessionRow>,
    /// Whether the project is a Git repository. When it isn't, worktree,
    /// branch, issue and pull-request controls are shown disabled.
    pub supports_git: bool,
    pub projects: Vec<ProjectView>,
    /// Self-test builds only: the probe's parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_test: Option<serde_json::Value>,
}

/// The selected (or first) session of a project, for a first paint.
async fn project_view_parts(
    project: &forge_session::OpenProjectRef,
) -> Result<(SessionId, DesktopEvent, Vec<SessionRow>), String> {
    let snapshots = project
        .snapshots()
        .await
        .map_err(|_| "the project has closed")?;
    let selected = project.selected_session_id().await.ok().flatten();
    let snapshot = match selected.and_then(|id| snapshots.iter().find(|s| s.task.session_id == id))
    {
        Some(snapshot) => snapshot,
        None => snapshots.first().ok_or("the project has no sessions")?,
    };
    Ok((
        snapshot.task.session_id,
        session_event(snapshot),
        session_rows(&snapshots),
    ))
}

/// Stream the project on screen into `channel` until the window goes away,
/// and every project's summary and new requests alongside.
#[tauri::command]
pub async fn subscribe(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
    channel: Channel<DesktopEvent>,
) -> Result<Hello, String> {
    let project = state.active();
    let (session_id, initial, sessions) = project_view_parts(&project).await?;
    let workspace = state.projects.workspace();
    let projects = project_views(&workspace.borrow());
    tauri::async_runtime::spawn(forward_workspace(
        app.clone(),
        channel.clone(),
        workspace,
        state.active.subscribe(),
    ));
    #[cfg(feature = "self-test")]
    let probe = state.self_test.clone();
    let active = state.active.subscribe();
    tauri::async_runtime::spawn(async move {
        // Force Tauri's large-message fetch path before the normal event
        // stream: blocking it stalls delivery of every subsequent frame.
        #[cfg(feature = "self-test")]
        if probe.is_some()
            && channel
                .send(DesktopEvent::Stream {
                    session_id: SessionId::nil(),
                    event: serde_json::json!({
                        "type": "channel_transport_probe",
                        "padding": "x".repeat(16 * 1024),
                    }),
                })
                .is_err()
        {
            return;
        }
        forward_active_project(
            channel,
            active,
            #[cfg(feature = "self-test")]
            probe,
        )
        .await;
    });
    #[cfg(feature = "self-test")]
    let self_test = state.self_test.as_ref().map(|probe| probe.hello());
    #[cfg(not(feature = "self-test"))]
    let self_test = None;
    Ok(Hello {
        project_id: project.project.id,
        session_id,
        initial,
        sessions,
        supports_git: project.handle.supports_git(),
        projects,
        self_test,
    })
}

/// Forward the on-screen project's events. On a switch, follow the new
/// project and send its roster, selection and selected session.
async fn forward_active_project(
    channel: Channel<DesktopEvent>,
    mut active: tokio::sync::watch::Receiver<forge_session::OpenProjectRef>,
    #[cfg(feature = "self-test")] probe: Option<std::sync::Arc<crate::self_test::Probe>>,
) {
    let mut switched = false;
    loop {
        let project = active.borrow_and_update().clone();
        let mut events = project.handle.subscribe();
        if switched {
            let header = DesktopEvent::ProjectSwitched {
                project_id: project.project.id,
                supports_git: project.handle.supports_git(),
            };
            if channel.send(header).is_err() || !resend_view(&channel, &project).await {
                return;
            }
        }
        switched = true;
        // The session on screen: only its tokens and snapshots are sent.
        let mut on_screen = project.selected_session_id().await.ok().flatten();
        // An event read while coalescing deltas, handled next.
        let mut held: Option<Result<SupervisorEvent, tokio::sync::broadcast::error::RecvError>> =
            None;
        loop {
            let event = match held.take() {
                Some(event) => event,
                None => tokio::select! {
                    changed = active.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        break;
                    }
                    event = events.recv() => event,
                },
            };
            // Join the deltas already queued behind this one into one
            // message: a token burst costs one IPC send, not one per token.
            let event = match event {
                Ok(SupervisorEvent::Stream {
                    session_id,
                    event: ModelStreamEvent::TextDelta { mut text },
                }) if Some(session_id) == on_screen => {
                    while text.len() < MAX_COALESCED_DELTA {
                        match events.try_recv() {
                            Ok(next) => match delta_text(&next, session_id) {
                                Some(more) => text.push_str(more),
                                None => {
                                    held = Some(Ok(next));
                                    break;
                                }
                            },
                            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(missed)) => {
                                held = Some(Err(tokio::sync::broadcast::error::RecvError::Lagged(
                                    missed,
                                )));
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    Ok(SupervisorEvent::Stream {
                        session_id,
                        event: ModelStreamEvent::TextDelta { text },
                    })
                }
                other => other,
            };
            if let Ok(SupervisorEvent::Selected(session_id)) = &event {
                on_screen = *session_id;
            }
            let event = match event {
                Ok(event) => route(event, on_screen),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    Some(DesktopEvent::Lagged { missed })
                }
                // The project closed under us; wait for the next switch.
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    if active.changed().await.is_err() {
                        return;
                    }
                    break;
                }
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
            // A fast stream can overrun the 512-event broadcast buffer. The
            // dropped events may include the turn's final SessionUpdated, so
            // re-send the live roster and selection after the notice.
            let lagged = matches!(&event, DesktopEvent::Lagged { .. });
            if channel.send(event).is_err() {
                // The webview is gone.
                return;
            }
            if lagged && !resend_view(&channel, &project).await {
                return;
            }
            // Selecting emits only the id, but the page needs that session's
            // transcript. Send it right after the selection.
            if let Some(session_id) = selected {
                if let Some(snapshot) = selected_snapshot(&project, session_id).await {
                    #[cfg(feature = "self-test")]
                    if let Some(probe) = &probe {
                        probe.delivering(&snapshot);
                    }
                    if channel.send(snapshot).is_err() {
                        return;
                    }
                }
            }
        }
    }
}

/// Send a project's roster, selection and selected session. `false` once
/// the webview is gone.
async fn resend_view(
    channel: &Channel<DesktopEvent>,
    project: &forge_session::OpenProjectRef,
) -> bool {
    let Ok(snapshots) = project.snapshots().await else {
        return true;
    };
    if channel
        .send(DesktopEvent::Roster {
            sessions: session_rows(&snapshots),
        })
        .is_err()
    {
        return false;
    }
    if let Ok(Some(session_id)) = project.selected_session_id().await {
        if channel
            .send(DesktopEvent::Selected {
                session_id: Some(session_id),
            })
            .is_err()
        {
            return false;
        }
        if let Ok(Some(snapshot)) = project.snapshot(session_id).await {
            if channel.send(session_event(&snapshot)).is_err() {
                return false;
            }
        }
    }
    true
}

/// Forward every project's summary, keep the badge equal to the number of
/// pending requests, and announce new ones once (D10, D11).
async fn forward_workspace(
    app: tauri::AppHandle,
    channel: Channel<DesktopEvent>,
    mut workspace: tokio::sync::watch::Receiver<forge_session::WorkspaceState>,
    active: tokio::sync::watch::Receiver<forge_session::OpenProjectRef>,
) {
    use tauri::Manager;
    // Requests and errors already there when the window opens are counted
    // but never announced.
    let mut previous = workspace.borrow().clone();
    let mut seen_errors = std::collections::HashMap::new();
    fresh_project_errors(&mut seen_errors, &previous, active.borrow().project.id);
    loop {
        let current = workspace.borrow_and_update().clone();
        let fresh = forge_session::new_attention(&previous, &current);
        let window = app.get_webview_window("main");
        let pending: usize = current
            .projects
            .iter()
            .map(|summary| summary.state.attention.len())
            .sum();
        if let Some(window) = &window {
            let _ = window.set_badge_count((pending > 0).then_some(pending as i64));
        }
        let project = active.borrow().clone();
        if !fresh.is_empty() {
            let focused = window
                .as_ref()
                .and_then(|window| window.is_focused().ok())
                .unwrap_or(false);
            let on_screen = project.selected_session_id().await.ok().flatten();
            let sound = fresh.iter().any(|(project_id, item)| {
                !(focused
                    && *project_id == project.project.id
                    && Some(item.session_id) == on_screen)
            });
            if !focused {
                if let Some(window) = &window {
                    let _ = window
                        .request_user_attention(Some(tauri::UserAttentionType::Informational));
                }
            }
            let event = DesktopEvent::AttentionNew {
                count: fresh.len(),
                sound,
            };
            if channel.send(event).is_err() {
                return;
            }
        }
        for event in fresh_project_errors(&mut seen_errors, &current, project.project.id) {
            if channel.send(event).is_err() {
                return;
            }
        }
        let event = DesktopEvent::Workspace {
            active: project.project.id,
            projects: project_views(&current),
        };
        if channel.send(event).is_err() {
            return;
        }
        previous = current;
        if workspace.changed().await.is_err() {
            return;
        }
    }
}

/// Why a project command failed, in a shape the page can act on.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProjectFailure {
    /// `needs_trust`, `already_owned`, `busy`, `root_missing`, `open`,
    /// `on_screen` or `error`.
    pub kind: &'static str,
    /// The project the command was about; `None` before one is known (the
    /// folder picker).
    pub project_id: Option<forge_session::ProjectId>,
    pub message: String,
    /// For `busy`: the sessions that would lose work, for the close dialog.
    pub sessions: Vec<BusySession>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BusySession {
    pub session_id: SessionId,
    pub label: String,
    pub turn_state: &'static str,
}

impl ProjectFailure {
    fn other(message: impl Into<String>) -> Self {
        Self {
            kind: "error",
            project_id: None,
            message: message.into(),
            sessions: Vec::new(),
        }
    }

    /// Attach the project, and name it in the message.
    fn about(mut self, state: &DesktopState, project_id: forge_session::ProjectId) -> Self {
        self.project_id = Some(project_id);
        let name = state
            .projects
            .workspace()
            .borrow()
            .projects
            .iter()
            .find(|summary| summary.project.id == project_id)
            .map(|summary| summary.project.name.clone());
        if let Some(name) = name {
            self.message = format!("{name}: {}", self.message);
        }
        self
    }
}

impl From<forge_session::ProjectError> for ProjectFailure {
    fn from(error: forge_session::ProjectError) -> Self {
        use forge_session::ProjectError;
        let kind = match &error {
            ProjectError::NeedsTrust(_) => "needs_trust",
            ProjectError::AlreadyOwned { .. } => "already_owned",
            ProjectError::Busy { .. } => "busy",
            ProjectError::RootMissing(_) => "root_missing",
            ProjectError::Open => "open",
            _ => "error",
        };
        let sessions = match &error {
            ProjectError::Busy { sessions } => sessions
                .iter()
                .map(|(session_id, turn_state)| BusySession {
                    session_id: *session_id,
                    label: String::new(),
                    turn_state: turn_state.label(),
                })
                .collect(),
            _ => Vec::new(),
        };
        Self {
            kind,
            project_id: None,
            message: error.to_string(),
            sessions,
        }
    }
}

/// Why a session command failed, in a shape the page can act on. The wire
/// shape matches [`ProjectFailure`]'s `kind` and `message`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SessionFailure {
    /// `unknown_session` (no open project knows the id: the page drops the
    /// row), `shutting_down` (the project closed underneath the command) or
    /// `error`.
    pub kind: &'static str,
    pub message: String,
}

impl From<forge_session::TargetError> for SessionFailure {
    fn from(error: forge_session::TargetError) -> Self {
        use forge_session::TargetError;
        let kind = match &error {
            TargetError::UnknownSession(_) => "unknown_session",
            TargetError::ShuttingDown => "shutting_down",
            TargetError::Supervisor(forge_session::RepositorySupervisorError::Closed) => {
                "shutting_down"
            }
            _ => "error",
        };
        Self {
            kind,
            message: error.to_string(),
        }
    }
}

impl From<forge_session::RepositorySupervisorError> for SessionFailure {
    fn from(error: forge_session::RepositorySupervisorError) -> Self {
        let kind = match &error {
            forge_session::RepositorySupervisorError::Closed => "shutting_down",
            _ => "error",
        };
        Self {
            kind,
            message: error.to_string(),
        }
    }
}

impl From<String> for SessionFailure {
    fn from(message: String) -> Self {
        Self {
            kind: "error",
            message,
        }
    }
}

impl From<&str> for SessionFailure {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

impl From<tokio::task::JoinError> for SessionFailure {
    fn from(error: tokio::task::JoinError) -> Self {
        error.to_string().into()
    }
}

/// Put a project on screen, opening it first if it is closed.
#[tauri::command]
pub async fn project_switch(
    state: State<'_, DesktopState>,
    project_id: forge_session::ProjectId,
) -> Result<(), ProjectFailure> {
    let about = |error| ProjectFailure::from(error).about(&state, project_id);
    state.projects.open(project_id).await.map_err(about)?;
    let project = state.projects.project(project_id).await.map_err(about)?;
    state.switch_to(project);
    Ok(())
}

/// Add a project from a folder the operator picks. Picking is the
/// confirmation. `None` when they cancel.
#[tauri::command]
pub async fn project_add(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
) -> Result<Option<forge_session::ProjectId>, ProjectFailure> {
    let Some(folder) = pick_folder(&app, "Add a project")
        .await
        .map_err(ProjectFailure::other)?
    else {
        return Ok(None);
    };
    Ok(Some(state.projects.add(folder).await?.id))
}

#[tauri::command]
pub async fn project_trust(
    state: State<'_, DesktopState>,
    project_id: forge_session::ProjectId,
) -> Result<(), ProjectFailure> {
    state
        .projects
        .trust(project_id)
        .await
        .map_err(|error| ProjectFailure::from(error).about(&state, project_id))
}

/// Close a project in the background. The project on screen can't be closed:
/// switch away first. `busy` lists the sessions that would lose work unless
/// `stop_running`. Returns the close's notices, empty for a clean close.
#[tauri::command]
pub async fn project_close(
    state: State<'_, DesktopState>,
    project_id: forge_session::ProjectId,
    stop_running: bool,
) -> Result<Vec<String>, ProjectFailure> {
    if state.active().project.id == project_id {
        return Err(ProjectFailure {
            kind: "on_screen",
            ..ProjectFailure::other("switch to another project before closing this one")
        }
        .about(&state, project_id));
    }
    // What the project holds open in this process, read while it can still
    // answer: its sessions' terminals and its folders' watched indexes.
    let (sessions, mut roots) = match state.projects.project(project_id).await {
        Ok(project) => {
            let records = project.session_records().await.unwrap_or_default();
            (
                records.iter().map(|record| record.session_id).collect(),
                records.into_iter().map(|record| record.workspace).collect(),
            )
        }
        Err(_) => (Vec::new(), Vec::new()),
    };
    if let Some(root) = state
        .projects
        .workspace()
        .borrow()
        .projects
        .iter()
        .find(|summary| summary.project.id == project_id)
        .map(|summary| summary.project.root.clone())
    {
        roots.push(root);
    }
    match state.projects.close(project_id, stop_running).await {
        // The notices say what the close could not finish cleanly (a session
        // still being stopped, say); the page shows them.
        Ok(notices) => {
            for session_id in sessions {
                state.shells.close(session_id);
            }
            state.indexes.forget(&roots);
            Ok(notices)
        }
        Err(error) => {
            let mut failure = ProjectFailure::from(error).about(&state, project_id);
            let workspace = state.projects.workspace().borrow().clone();
            let sessions = workspace
                .projects
                .iter()
                .filter(|summary| summary.project.id == project_id)
                .flat_map(|summary| &summary.state.sessions);
            for session in sessions {
                if let Some(busy) = failure
                    .sessions
                    .iter_mut()
                    .find(|busy| busy.session_id == session.session_id)
                {
                    busy.label = session.label.clone();
                }
            }
            Err(failure)
        }
    }
}

/// Unregister a closed project. Its folder and saved sessions stay.
#[tauri::command]
pub async fn project_remove(
    state: State<'_, DesktopState>,
    project_id: forge_session::ProjectId,
) -> Result<(), ProjectFailure> {
    state
        .projects
        .remove(project_id)
        .await
        .map_err(|error| ProjectFailure::from(error).about(&state, project_id))?;
    Ok(())
}

/// Point a project whose folder moved at its new location. `false` when the
/// operator cancels the picker.
#[tauri::command]
pub async fn project_relocate(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
    project_id: forge_session::ProjectId,
) -> Result<bool, ProjectFailure> {
    let Some(folder) = pick_folder(&app, "Locate the project folder")
        .await
        .map_err(ProjectFailure::other)?
    else {
        return Ok(false);
    };
    state
        .projects
        .relocate(project_id, folder)
        .await
        .map_err(|error| ProjectFailure::from(error).about(&state, project_id))?;
    Ok(true)
}

/// The snapshot behind a selection, read from the supervisor's live actors.
async fn selected_snapshot(
    project: &forge_session::OpenProjectRef,
    session_id: Option<SessionId>,
) -> Option<DesktopEvent> {
    let snapshot = project.snapshot(session_id?).await.ok()??;
    Some(session_event(&snapshot))
}

#[tauri::command]
pub async fn submit_prompt(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    text: String,
    attachment: Option<crate::workspace::PromptAttachment>,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    if text.trim().is_empty() {
        return Err("prompt is empty".into());
    }
    let text = match attachment {
        Some(attachment) => {
            let root = workspace_of(&state, session_id).await?;
            let context = tokio::task::spawn_blocking(move || {
                crate::workspace::attachment_text(&root, &attachment)
            })
            .await
            .map_err(SessionFailure::from)??;
            format!("{context}\n\n{text}")
        }
        None => text,
    };
    #[cfg(feature = "self-test")]
    if let Some(probe) = &state.self_test {
        probe.submitted();
    }
    project
        .handle
        .submit(SupervisorCommand::SubmitPrompt { session_id, text })
        // Fire and forget: the outcome arrives on the event channel.
        .map(drop)
        .map_err(SessionFailure::from)
}

/// Image bytes, not page-supplied filesystem paths, enter at this boundary.
#[tauri::command]
pub async fn submit_images(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    text: String,
    images: Vec<String>,
    attachment: Option<crate::workspace::PromptAttachment>,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    let root = {
        let snapshot = project
            .snapshot(session_id)
            .await?
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
            .map_err(SessionFailure::from)??;
            format!("{context}\n\n{text}")
        }
        None => text,
    };
    let attachments =
        tokio::task::spawn_blocking(move || crate::workspace::store_images(&root, &images))
            .await
            .map_err(SessionFailure::from)??;
    project
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
        .map_err(SessionFailure::from)
}

/// Create a session (its own managed worktree) and select it. The roster
/// and selection arrive on the event channel.
#[tauri::command]
pub async fn create_session(
    state: State<'_, DesktopState>,
    label: String,
    github_issue_number: Option<u64>,
) -> Result<(), String> {
    let project = state.active();
    let label = label.trim().to_string();
    if label.is_empty() {
        return Err("session name is empty".into());
    }
    project
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
) -> Result<DesktopEvent, SessionFailure> {
    let project = state.project_for(session_id).await?;
    project
        .handle
        .command(SupervisorCommand::SelectSession {
            session_id: Some(session_id),
        })
        .await?;
    selected_snapshot(&project, Some(session_id))
        .await
        .ok_or_else(|| "the selected session is gone".into())
}

/// Archive a session. The supervisor picks the next selection and publishes
/// the new roster.
#[tauri::command]
pub async fn archive_session(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    project
        .handle
        .command(SupervisorCommand::ArchiveSession { session_id })
        .await
        .map_err(SessionFailure::from)
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
    let project = state.active();
    let (workspace, label, branch) = attach_fields(&workspace, &label, &branch)?;
    project
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
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    let record = {
        project
            .session_records()
            .await?
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
        project
            .handle
            .command(SupervisorCommand::ArchiveSession { session_id })
            .await?;
    }
    project
        .handle
        .command(SupervisorCommand::RemoveManagedWorktree {
            session_id,
            discard_dirty,
        })
        .await?;
    // A dirty worktree leaves the session retained instead of removed: say so
    // rather than reporting a cleanup that never happened.
    let lifecycle = {
        project
            .session_records()
            .await?
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

/// The pending request the page is acting on, by id.
async fn pending_hitl(
    state: &DesktopState,
    session_id: SessionId,
    call_id: &str,
) -> Result<HitlPayload, SessionFailure> {
    let project = state.project_for(session_id).await?;
    let snapshot = project
        .snapshot(session_id)
        .await?
        .ok_or_else(|| "the session is gone".to_string())?;
    snapshot
        .session
        .pending_hitl
        .filter(|payload| payload.call_id == call_id)
        .ok_or_else(|| "that request is no longer pending".into())
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
    session_id: SessionId,
    call_id: String,
    decision: String,
    note: Option<String>,
    confirmed: bool,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
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
        other => return Err(format!("unknown decision `{other}`").into()),
    };
    project
        .handle
        .submit(SupervisorCommand::DecideApproval {
            session_id,
            expected_call_id: Some(call_id),
            decision,
            actor: "desktop".into(),
        })
        .map(drop)
        .map_err(SessionFailure::from)
}

/// A host the network gateway is holding a connection for.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostQuestionView {
    pub host: String,
    pub port: u16,
    /// The tool call that is waiting on it, when known.
    pub call_id: Option<String>,
    /// Set when the command wants to change something on `host` as the
    /// user, rather than reach it: what it is doing.
    pub write: Option<String>,
}

/// Decide a host a running command is waiting to reach: `allow_once`,
/// `allow_for_session`, `allow_always` (this project, from now on) or
/// `deny`. The command carries on, or sees the connection refused.
#[tauri::command]
pub async fn answer_host_question(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    host: String,
    decision: String,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    let decision = match decision.as_str() {
        "allow_once" => forge_core::HostDecision::AllowOnce,
        "allow_for_session" => forge_core::HostDecision::AllowForSession,
        "allow_always" => forge_core::HostDecision::AllowAlways,
        "deny" => forge_core::HostDecision::Deny,
        other => return Err(format!("unknown decision `{other}`").into()),
    };
    project
        .handle
        .submit(SupervisorCommand::AnswerHostQuestion {
            session_id,
            host,
            decision,
        })
        .map(drop)
        .map_err(SessionFailure::from)
}

/// Turn approve-all on or off for the selected session. Session scoped and
/// never persisted; the sandbox still prompts for an unconfined retry.
#[tauri::command]
pub async fn set_approve_all(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    on: bool,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    project
        .handle
        .submit(SupervisorCommand::SetApproveAll { session_id, on })
        .map(drop)
        .map_err(SessionFailure::from)
}

/// Forget every remembered pattern for the selected session.
#[tauri::command]
pub async fn clear_approvals(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    project
        .handle
        .submit(SupervisorCommand::ClearSessionApprovals { session_id })
        .map(drop)
        .map_err(SessionFailure::from)
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
    session_id: SessionId,
    call_id: String,
    answers: Vec<AnswerInput>,
    skip: bool,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    let pending = {
        project
            .snapshot(session_id)
            .await?
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
    project
        .handle
        .submit(SupervisorCommand::ResolveQuestion {
            session_id,
            expected_call_id: Some(call_id),
            answers,
            actor: "desktop".into(),
        })
        .map(drop)
        .map_err(SessionFailure::from)
}

/// The selected session's live snapshot, used to resync after the window
/// regains focus in case an event was missed while it was hidden.
#[tauri::command]
pub async fn current_session(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<Option<DesktopEvent>, SessionFailure> {
    let project = state.project_for(session_id).await?;
    Ok(selected_snapshot(&project, Some(session_id)).await)
}

/// Resume the selected session's stopped or interrupted turn.
#[tauri::command]
pub async fn continue_turn(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    project
        .handle
        .submit(SupervisorCommand::ContinueTurn { session_id })
        .map(drop)
        .map_err(SessionFailure::from)
}

/// Stop one session's turn, whether or not it is the selected one.
#[tauri::command]
pub async fn stop_session(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<DesktopEvent, SessionFailure> {
    let project = state.project_for(session_id).await?;
    project
        .handle
        .command(SupervisorCommand::StopTurn { session_id })
        .await?;
    selected_snapshot(&project, Some(session_id))
        .await
        .ok_or_else(|| "the stopped session is gone".into())
}

/// Stop managing a session without archiving it; it can be resumed later.
#[tauri::command]
pub async fn close_session(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    // The window always needs a selected session to resume from; closing the
    // last one would strand the durable row with no conversation to replace.
    let active = {
        project
            .snapshots()
            .await?
            .into_iter()
            .filter(|snapshot| snapshot.task.lifecycle == forge_session::SessionLifecycle::Active)
            .count()
    };
    if active <= 1 {
        return Err(
            "This is the last open session; start or resume another before closing it".into(),
        );
    }
    project
        .handle
        .command(SupervisorCommand::CloseSession { session_id })
        .await
        .map_err(SessionFailure::from)
}

/// Durable sessions in `session_id`'s workspace that this process has
/// closed. They are not live rows; resuming one replaces that conversation.
#[tauri::command]
pub async fn list_resumable_sessions(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<Vec<SessionRow>, SessionFailure> {
    let project = state.project_for(session_id).await?;
    let snapshots = project.snapshots().await?;
    let Some(active) = snapshots
        .iter()
        .find(|snapshot| snapshot.task.session_id == session_id)
    else {
        return Ok(Vec::new());
    };
    let root = &active.task.workspace;
    let live: std::collections::HashSet<SessionId> = snapshots
        .iter()
        .map(|snapshot| snapshot.task.session_id)
        .collect();
    Ok(project
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
pub async fn fork_session(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    project
        .handle
        .command(SupervisorCommand::ForkSession { session_id })
        .await
        .map_err(SessionFailure::from)
}

/// Switch to another durable session, closing nothing.
#[tauri::command]
pub async fn resume_session(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    current_session_id: SessionId,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    project
        .handle
        .command(SupervisorCommand::ResumeSession {
            current_session_id,
            session_id,
        })
        .await
        .map_err(SessionFailure::from)
}

#[tauri::command]
pub async fn rename_session(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    label: String,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    let label = label.trim().to_string();
    if label.is_empty() {
        return Err("session name is empty".into());
    }
    project
        .handle
        .command(SupervisorCommand::RenameSession { session_id, label })
        .await
        .map_err(SessionFailure::from)
}

/// Pin a session to a slot, or unpin it with no slot.
#[tauri::command]
pub async fn pin_session(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    slot: Option<u8>,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    project
        .handle
        .command(SupervisorCommand::PinSession {
            session_id,
            slot,
            swap: false,
        })
        .await
        .map_err(SessionFailure::from)
}

/// Remove one queued prompt, bound to the id the page showed.
#[tauri::command]
pub async fn cancel_queued(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    queue_id: u64,
    source: QueueSource,
    position: usize,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    project
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
        .map_err(SessionFailure::from)
}

/// Compact the selected session's context now.
#[tauri::command]
pub async fn compact_context(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    project
        .handle
        .submit(SupervisorCommand::CompactContext { session_id })
        .map(drop)
        .map_err(SessionFailure::from)
}

/// Turn extended thinking on or off for the selected session.
#[tauri::command]
pub async fn set_thinking(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    enabled: bool,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    project
        .handle
        .submit(SupervisorCommand::SetThinking {
            session_id,
            enabled,
        })
        .map(drop)
        .map_err(SessionFailure::from)
}

/// Stop one run of a background task. Scoping by `run_id` keeps a late click
/// from cancelling a replacement run that reused the task id.
#[tauri::command]
pub async fn cancel_background(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    task_id: u64,
    run_id: String,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    let run_id: forge_core::BackgroundRunId = run_id
        .trim()
        .parse()
        .map_err(|_| "that background run is no longer valid".to_string())?;
    project
        .handle
        .submit(SupervisorCommand::CancelBackgroundRun {
            session_id,
            task_id: forge_types::BackgroundTaskId(task_id),
            run_id,
        })
        .map(drop)
        .map_err(SessionFailure::from)
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
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    let request = {
        project
            .snapshot(session_id)
            .await?
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
    project
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
        .map_err(SessionFailure::from)
}

/// A read-only view of one background child's saved transcript.
#[tauri::command]
pub async fn child_transcript(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    task_id: u64,
    child_session_id: SessionId,
) -> Result<Vec<MessageView>, SessionFailure> {
    let project = state.project_for(session_id).await?;
    let journal = {
        let child = project
            .snapshot(session_id)
            .await?
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
        project.journal_dir().await?
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
) -> Result<MessageView, SessionFailure> {
    let project = state.project_for(session_id).await?;
    let snapshot = project
        .snapshot(session_id)
        .await?
        .ok_or("That session is no longer live")?;
    snapshot
        .transcript
        .messages()
        .iter()
        .filter_map(|message| message_view(message, true))
        .nth(index)
        .ok_or_else(|| "that message is no longer in the transcript".into())
}

/// One session's workspace root, by id.
async fn workspace_of(
    state: &DesktopState,
    session_id: SessionId,
) -> Result<std::path::PathBuf, SessionFailure> {
    let project = state.project_for(session_id).await?;
    project
        .snapshot(session_id)
        .await?
        .map(|snapshot| snapshot.session.workspace_root)
        .ok_or_else(|| "the session is gone".into())
}

/// One folder of a session's workspace (`""` is the root). The tree loads a
/// folder when it is expanded, so no listing walks the whole workspace.
#[tauri::command]
pub async fn list_dir(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    path: String,
) -> Result<crate::workspace::DirListing, SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::list_dir(&root, &path))
        .await?
        .map_err(SessionFailure::from)
}

/// Files in a session's workspace whose path fuzzily matches `query`, best
/// first (Quick Open and the Files filter).
#[tauri::command]
pub async fn find_files(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    query: String,
) -> Result<Vec<crate::workspace::FoundFile>, SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    let indexes = state.indexes.clone();
    tokio::task::spawn_blocking(move || {
        crate::workspace::find(&*indexes.for_root(&root)?, &query, 50)
    })
    .await?
    .map_err(SessionFailure::from)
}

/// One UTF-8 file from a session's workspace.
#[tauri::command]
pub async fn read_file(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    path: String,
) -> Result<crate::workspace::FileContents, SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::read(&root, &path))
        .await?
        .map_err(SessionFailure::from)
}

/// An image as a size-capped data URL, for the file pane and transcript chips.
#[tauri::command]
pub async fn read_image(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    path: String,
) -> Result<crate::workspace::ImageContents, SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::read_image(&root, &path))
        .await?
        .map_err(SessionFailure::from)
}

/// Branch and changed files for a session's workspace.
#[tauri::command]
pub async fn git_status(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<crate::workspace::WorkspaceStatus, SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::status(&root))
        .await?
        .map_err(SessionFailure::from)
}

/// The worktree diff of one file, with untracked files shown as additions.
#[tauri::command]
pub async fn git_diff(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    path: String,
    side: Option<crate::workspace::DiffSide>,
) -> Result<crate::workspace::FileDiff, SessionFailure> {
    let side = side.unwrap_or_default();
    if side == crate::workspace::DiffSide::LastTurn {
        let cards = last_turn_cards(&state, session_id).await?;
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
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::diff(&root, &path, side))
        .await?
        .map_err(SessionFailure::from)
}

/// The files the most recent assistant turn wrote, read from the transcript's
/// own diff cards so "last turn" stays honest after the tree moves on.
async fn last_turn_cards(
    state: &DesktopState,
    session_id: SessionId,
) -> Result<Vec<(String, Vec<String>)>, SessionFailure> {
    let project = state.project_for(session_id).await?;
    let snapshot = project
        .snapshot(session_id)
        .await?
        .ok_or("No live session to review")?;
    Ok(crate::last_turn::diff_cards(snapshot.transcript.messages()))
}

/// Paths written during the most recent assistant turn.
#[tauri::command]
pub async fn git_last_turn_files(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<Vec<String>, SessionFailure> {
    Ok(last_turn_cards(&state, session_id)
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
) -> Result<String, SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::git_operation(&root, operation))
        .await?
        .map_err(SessionFailure::from)
}

/// Create an empty file in a session's workspace.
#[tauri::command]
pub async fn create_file(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    path: String,
) -> Result<(), SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::create_file(&root, &path))
        .await?
        .map_err(SessionFailure::from)
}

#[tauri::command]
pub async fn create_dir(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    path: String,
) -> Result<(), SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::create_dir(&root, &path))
        .await?
        .map_err(SessionFailure::from)
}

/// Rename or move a path inside a session's workspace.
#[tauri::command]
pub async fn rename_path(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    from: String,
    to: String,
) -> Result<(), SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::rename(&root, &from, &to))
        .await?
        .map_err(SessionFailure::from)
}

/// Delete one file or empty directory.
#[tauri::command]
pub async fn delete_path(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    path: String,
) -> Result<(), SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::delete(&root, &path))
        .await?
        .map_err(SessionFailure::from)
}

/// Content search across a session's workspace, through its index.
#[tauri::command]
pub async fn search_files(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    query: String,
) -> Result<Vec<crate::workspace::SearchHit>, SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    let indexes = state.indexes.clone();
    tokio::task::spawn_blocking(move || {
        crate::workspace::search(&*indexes.for_root(&root)?, &query)
    })
    .await?
    .map_err(SessionFailure::from)
}

#[tauri::command]
pub async fn git_stage(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    path: String,
) -> Result<(), SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::stage(&root, &path))
        .await?
        .map_err(SessionFailure::from)
}

#[tauri::command]
pub async fn git_unstage(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    path: String,
) -> Result<(), SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::unstage(&root, &path))
        .await?
        .map_err(SessionFailure::from)
}

/// Commit what is staged.
#[tauri::command]
pub async fn git_commit(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    message: String,
) -> Result<String, SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::commit(&root, &message))
        .await?
        .map_err(SessionFailure::from)
}

#[tauri::command]
pub async fn git_branches(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<Vec<String>, SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::branches(&root))
        .await?
        .map_err(SessionFailure::from)
}

#[tauri::command]
pub async fn git_switch_branch(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    name: String,
) -> Result<(), SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::switch_branch(&root, &name))
        .await?
        .map_err(SessionFailure::from)
}

#[tauri::command]
pub async fn git_create_branch(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    name: String,
) -> Result<(), SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::create_branch(&root, &name))
        .await?
        .map_err(SessionFailure::from)
}

#[tauri::command]
pub async fn git_merge_branch(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    name: String,
) -> Result<(), SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::merge_branch(&root, &name))
        .await?
        .map_err(SessionFailure::from)
}

#[tauri::command]
pub async fn git_rename_branch(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    from: String,
    to: String,
) -> Result<(), SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::rename_branch(&root, &from, &to))
        .await?
        .map_err(SessionFailure::from)
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
        // A provider connected elsewhere (an imported sign-in, a migrated
        // credential file) may have no cached catalog here. Refresh the stale
        // ones first; otherwise its models never become selectable.
        for profile in registry.profiles() {
            if !cache.is_fresh(&profile.id) {
                let _ = forge_connect::refresh_profile_catalog(profile, &store, &cache);
            }
        }
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
pub async fn session_details(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<serde_json::Value, SessionFailure> {
    let project = state.project_for(session_id).await?;
    let details = {
        project
            .snapshot(session_id)
            .await?
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
    }).await.map_err(SessionFailure::from)
}

/// Provider setup never sends a secret through the webview.
#[tauri::command]
pub async fn list_providers() -> Result<Vec<crate::providers::ProviderView>, String> {
    tokio::task::spawn_blocking(crate::providers::list)
        .await
        .map_err(|error| error.to_string())?
}

/// Installed themes, from the same built-ins and drop-in directories as the
/// terminal. The page resolves `system` itself because only it can observe the
/// OS preference.
#[tauri::command]
pub async fn list_themes(
    state: State<'_, DesktopState>,
    session_id: Option<SessionId>,
) -> Result<Vec<crate::themes::ThemeView>, String> {
    // A session's workspace can carry its own themes; before one is on
    // screen only the user-level ones are listed.
    let workspace = match session_id {
        Some(session_id) => workspace_of(&state, session_id).await.ok(),
        None => None,
    };
    tokio::task::spawn_blocking(move || crate::themes::list(workspace.as_deref()))
        .await
        .map_err(|error| error.to_string())
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
    state.reload_providers().await?;
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
    state.reload_providers().await?;
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
    state.reload_providers().await?;
    Ok(message)
}

/// Push the linked issue branch and open its pull request.
#[tauri::command]
pub async fn github_create_pr(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    issue_number: u64,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    project
        .handle
        .command(SupervisorCommand::CreateIssuePullRequest {
            session_id,
            issue_number,
        })
        .await
        .map_err(SessionFailure::from)
}

/// Re-read the linked pull request's checks and review state.
#[tauri::command]
pub async fn github_refresh_pr(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    issue_number: u64,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    project
        .handle
        .command(SupervisorCommand::RefreshIssuePullRequest {
            session_id,
            issue_number,
        })
        .await
        .map_err(SessionFailure::from)
}

/// Queue the agent's next turn from the linked pull request's failed checks.
#[tauri::command]
pub async fn github_apply_feedback(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    issue_number: u64,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    project
        .handle
        .command(SupervisorCommand::ApplyIssuePullRequestFeedback {
            session_id,
            issue_number,
        })
        .await
        .map_err(SessionFailure::from)
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
    state.reload_providers().await?;
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
    state.reload_providers().await?;
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
    state.reload_providers().await?;
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
    state.reload_providers().await
}

/// Switch the selected session's model (and reasoning effort).
#[tauri::command]
pub async fn set_model(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    model_id: String,
    profile_id: String,
    effort: Option<String>,
) -> Result<(), SessionFailure> {
    let project = state.project_for(session_id).await?;
    // The supervisor derives the model's capabilities with the switch.
    project
        .handle
        .command(SupervisorCommand::SetModel {
            session_id,
            model_id,
            profile_id,
            reasoning_effort: effort,
        })
        .await
        .map_err(SessionFailure::from)
}

/// Save an edited file back into a session's workspace. Refused if the file
/// changed since `read_file` returned that fingerprint.
#[tauri::command]
pub async fn write_file(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    path: String,
    text: String,
    fingerprint: String,
    crlf: bool,
) -> Result<crate::workspace::FileContents, SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || {
        crate::workspace::write(&root, &path, &text, &fingerprint, crlf)
    })
    .await?
    .map_err(SessionFailure::from)
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

#[tauri::command]
pub async fn github_issues(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<Vec<serde_json::Value>, SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::github::issues(&root))
        .await?
        .map_err(SessionFailure::from)
}

#[tauri::command]
pub async fn github_details(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    issue_number: Option<u64>,
    pr_number: Option<u64>,
) -> Result<serde_json::Value, SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::github::details(&root, issue_number, pr_number))
        .await?
        .map_err(SessionFailure::from)
}

#[tauri::command]
pub async fn github_logs(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    run_id: u64,
    job_id: Option<u64>,
) -> Result<serde_json::Value, SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
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
    .await?
    .map_err(|error: String| error.into())
}

/// Discard the working-tree change to one tracked file.
#[tauri::command]
pub async fn git_restore(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    path: String,
) -> Result<(), SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::restore(&root, &path))
        .await?
        .map_err(SessionFailure::from)
}

/// Reverse-apply one working-tree hunk. The index is the position in the
/// file's HEAD-to-worktree diff, which is exactly what the review shows.
#[tauri::command]
pub async fn git_discard_hunk(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    path: String,
    hunk_index: usize,
) -> Result<(), SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    tokio::task::spawn_blocking(move || crate::workspace::discard_hunk(&root, &path, hunk_index))
        .await?
        .map_err(SessionFailure::from)
}

/// Open (or restart) the active session's interactive shell.
#[tauri::command]
pub async fn shell_open(
    state: State<'_, DesktopState>,
    channel: Channel<crate::shell::ShellEvent>,
    session_id: SessionId,
    cols: u16,
    rows: u16,
) -> Result<(), SessionFailure> {
    let root = workspace_of(&state, session_id).await?;
    state
        .shells
        .open(session_id, &root, cols, rows, move |event| {
            channel.send(event).is_ok()
        })
        .map_err(SessionFailure::from)
}

/// Keystrokes from the page, as UTF-8.
#[tauri::command]
pub async fn shell_write(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    data: String,
) -> Result<(), SessionFailure> {
    state
        .shells
        .write(session_id, data.as_bytes())
        .map_err(SessionFailure::from)
}

#[tauri::command]
pub async fn shell_resize(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    cols: u16,
    rows: u16,
) -> Result<(), SessionFailure> {
    state
        .shells
        .resize(session_id, cols, rows)
        .map_err(SessionFailure::from)
}

#[tauri::command]
pub async fn shell_close(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<(), SessionFailure> {
    state.shells.close(session_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picker_projects(scratch: &std::path::Path) -> forge_session::ProjectServiceHandle {
        forge_session::ProjectService::spawn(forge_session::ProjectServiceOptions {
            registry: forge_session::ProjectRegistry::at(scratch.join("projects.toml")),
            trust_store: Some(scratch.join("trust.toml")),
            load_config: std::sync::Arc::new(|_root| Err("not opened in this test".into())),
        })
        .unwrap()
    }

    /// The picker is open until a folder has actually opened: with no folder
    /// yet, and while one is only being asked about on the trust screen.
    #[tokio::test]
    async fn another_folder_can_be_chosen_until_one_has_opened() {
        let scratch = std::env::temp_dir().join(format!(
            "forge-desktop-choose-{}",
            forge_types::SessionId::new_v4()
        ));
        std::fs::create_dir_all(&scratch).unwrap();
        let projects = picker_projects(&scratch);

        let asking = TrustState::pending(
            projects.clone(),
            scratch.join("wrong-pick"),
            Pending::Confirm(scratch.join("wrong-pick")),
        );
        assert!(asking.can_choose_folder());
        asking.fail("could not open it".into());
        asking.forget_pending();
        let view = trust_view(&asking);
        assert!(view.needs_folder && !view.needs_trust && view.error.is_empty());
        assert!(asking.can_choose_folder());

        let opened = TrustState::ready(projects.clone(), scratch.join("opened"));
        assert!(!opened.can_choose_folder());
        projects.shutdown().await.unwrap();
        let _ = std::fs::remove_dir_all(scratch);
    }

    /// An untrusted pick registers nothing: backing out of the trust screen
    /// must leave no project behind to capture later launches.
    #[tokio::test]
    async fn picking_an_untrusted_folder_registers_nothing_until_it_is_trusted() {
        let scratch = std::env::temp_dir().join(format!(
            "forge-desktop-pick-{}",
            forge_types::SessionId::new_v4()
        ));
        let folder = scratch.join("picked");
        std::fs::create_dir_all(&folder).unwrap();
        let projects = picker_projects(&scratch);

        let picked = picked_folder(&projects, folder.clone(), |_| false)
            .await
            .unwrap();
        assert!(matches!(picked, Picked::AskFirst(_)), "{picked:?}");
        assert!(projects.workspace().borrow().projects.is_empty());

        // The same folder, once trusted, is registered by the pick.
        let picked = picked_folder(&projects, folder, |_| true).await.unwrap();
        let Picked::Project(project) = picked else {
            panic!("a trusted new folder is registered: {picked:?}");
        };
        assert_eq!(project.name, "picked");
        assert_eq!(projects.workspace().borrow().projects.len(), 1);
        projects.shutdown().await.unwrap();
        let _ = std::fs::remove_dir_all(scratch);
    }

    /// The page shows what the operator should read: the reason a turn
    /// failed without its internal marker, and no empty assistant row for a
    /// reply that was only tool calls.
    #[test]
    fn the_transcript_view_hides_the_failure_marker_and_empty_replies() {
        use forge_types::{Message, MessageRole};
        let failed = Message::new(
            MessageRole::Assistant,
            format!(
                "{}Forge stopped after repeated denied approvals.",
                forge_core::TURN_FAILED_MARKER
            ),
        );
        for full in [false, true] {
            assert_eq!(
                message_view(&failed, full).unwrap().text,
                "Forge stopped after repeated denied approvals."
            );
        }

        let tool_calls_only = Message::new(MessageRole::Assistant, "");
        assert!(message_view(&tool_calls_only, false).is_none());
        assert!(message_view(&tool_calls_only, true).is_none());

        let mut thinking_only = Message::new(MessageRole::Assistant, "");
        thinking_only.thinking = Some("weighing the options".into());
        assert!(message_view(&thinking_only, false).is_some());

        // Other roles are untouched: an empty tool result is still a row.
        assert!(message_view(&Message::new(MessageRole::Tool, ""), false).is_some());
        let quoted = Message::new(MessageRole::User, forge_core::TURN_FAILED_MARKER);
        assert_eq!(
            message_view(&quoted, false).unwrap().text,
            forge_core::TURN_FAILED_MARKER
        );
    }

    #[test]
    fn a_wait_without_a_card_reaches_the_page_as_words() {
        use forge_session::WaitingOn;
        assert_eq!(waiting_view(&WaitingOn::Reply).kind, "reply");
        let key = waiting_view(&WaitingOn::Configuration("GITHUB_TOKEN".into()));
        assert_eq!(
            (key.kind, key.detail.as_str()),
            ("configuration", "GITHUB_TOKEN")
        );
        let outside = waiting_view(&WaitingOn::ExternalAction("Merge the PR.".into()));
        assert_eq!(
            (outside.kind, outside.detail.as_str()),
            ("external", "Merge the PR.")
        );
        assert_eq!(waiting_view(&WaitingOn::Other).kind, "other");
    }

    #[test]
    fn a_session_failure_names_what_the_page_can_act_on() {
        use forge_session::{RepositorySupervisorError, TargetError};
        let id = SessionId::from_u128(7);
        let unknown = SessionFailure::from(TargetError::UnknownSession(id));
        assert_eq!(unknown.kind, "unknown_session");
        assert_eq!(unknown.message, "that session no longer exists");
        assert_eq!(
            SessionFailure::from(TargetError::ShuttingDown).kind,
            "shutting_down"
        );
        assert_eq!(
            SessionFailure::from(RepositorySupervisorError::Closed).kind,
            "shutting_down"
        );
        assert_eq!(
            SessionFailure::from(TargetError::Supervisor(RepositorySupervisorError::Closed)).kind,
            "shutting_down"
        );
        // Everything else is shown as its message.
        let plain = SessionFailure::from("Session is gone");
        assert_eq!(
            (plain.kind, plain.message.as_str()),
            ("error", "Session is gone")
        );
        assert_eq!(
            SessionFailure::from(RepositorySupervisorError::NoActor(id)).kind,
            "error"
        );
        // The wire shape the page reads.
        assert_eq!(
            serde_json::to_value(&unknown).unwrap(),
            serde_json::json!({"kind": "unknown_session", "message": "that session no longer exists"})
        );
    }

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
                github_retry_attempts: 0,
                github_tests_passed: false,
            },
            session: forge_session::SessionSnapshot::default(),
            transcript: forge_session::TranscriptSnapshot::default(),
            details: None,
            queued_prompts: Vec::new(),
            interrupted_prompts: Vec::new(),
            host_questions: Vec::new(),
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

    #[test]
    fn session_snapshots_include_skill_completion_metadata() {
        let mut snapshot = snapshot(
            SessionId::new_v4(),
            "skills",
            SessionLifecycle::Active,
            forge_session::SupervisorTurnState::Idle,
        );
        snapshot.details = Some(forge_session::SessionDetailsSnapshot {
            journal_dir: Default::default(),
            active_model: String::new(),
            active_profile_id: String::new(),
            reasoning_effort: None,
            thinking_enabled: false,
            image_input_supported: false,
            token_usage_report: Default::default(),
            tools: Vec::new(),
            skills: vec!["review".into()],
            skill_descriptions: vec![("review".into(), "Review changes".into())],
            context_state: Default::default(),
            compaction: Default::default(),
            queue: Vec::new(),
            background: Vec::new(),
            session_pattern_allow_count: 0,
        });
        let json = serde_json::to_value(session_event(&snapshot)).unwrap();
        assert_eq!(
            json["skill_descriptions"],
            serde_json::json!([["review", "Review changes"]])
        );
        snapshot.details = None;
        let json = serde_json::to_value(session_event(&snapshot)).unwrap();
        assert_eq!(json["skill_descriptions"], serde_json::json!([]));
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
    fn sandbox_escalations_and_host_grants_need_a_second_confirmation() {
        for privileged in [payload(true, None), payload(false, Some("example.com"))] {
            assert!(requires_native_confirm(&privileged), "{privileged:?}");
        }
        assert!(!requires_native_confirm(&payload(false, None)));
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

    /// Only the session on screen gets tokens and full snapshots; another
    /// session's update becomes a small state change, its tokens are dropped.
    #[test]
    fn only_the_session_on_screen_gets_tokens_and_transcripts() {
        let (shown, other) = (SessionId::new_v4(), SessionId::new_v4());
        let delta = |session_id| SupervisorEvent::Stream {
            session_id,
            event: ModelStreamEvent::TextDelta { text: "hi".into() },
        };
        assert!(matches!(
            route(delta(shown), Some(shown)),
            Some(DesktopEvent::Stream { .. })
        ));
        assert_eq!(route(delta(other), Some(shown)), None);
        assert_eq!(
            route(
                SupervisorEvent::Stream {
                    session_id: shown,
                    event: ModelStreamEvent::ThinkingDelta { text: "hm".into() },
                },
                Some(shown)
            ),
            None,
            "the page streams text deltas only"
        );

        let update = |id| {
            SupervisorEvent::SessionUpdated(Box::new(snapshot(
                id,
                "s",
                SessionLifecycle::Active,
                forge_session::SupervisorTurnState::Running,
            )))
        };
        assert!(matches!(
            route(update(shown), Some(shown)),
            Some(DesktopEvent::Session { .. })
        ));
        assert_eq!(
            route(update(other), Some(shown)),
            Some(DesktopEvent::SessionState {
                session_id: other,
                turn_state: "running"
            })
        );
    }

    #[test]
    fn errors_from_background_projects_are_shown_once() {
        use forge_session::{ProjectId, ProjectNotice, ProjectSummary, WorkspaceState};
        let project = |name: &str, seq: Option<u64>| ProjectSummary {
            project: forge_session::Project {
                id: ProjectId::new(),
                root: format!("/{name}").into(),
                name: name.into(),
                added_at: chrono::Utc::now(),
            },
            open: true,
            state: Default::default(),
            last_error: seq.map(|seq| ProjectNotice {
                seq,
                session_id: None,
                message: format!("{name} broke ({seq})"),
            }),
        };
        let mut workspace = WorkspaceState {
            version: 1,
            projects: vec![
                project("api", Some(1)),
                project("notes", Some(1)),
                project("sdk", None),
            ],
        };
        let (api, notes) = (
            workspace.projects[0].project.id,
            workspace.projects[1].project.id,
        );
        let mut seen = std::collections::HashMap::new();
        // Errors already there when the window opens are not replayed.
        fresh_project_errors(&mut seen, &workspace, api);

        workspace.projects[0].last_error.as_mut().unwrap().seq = 2;
        workspace.projects[1].last_error = Some(ProjectNotice {
            seq: 2,
            session_id: None,
            message: "notes broke (2)".into(),
        });
        let fresh = fresh_project_errors(&mut seen, &workspace, api);
        // Only the background project; the on-screen one sends its own.
        assert_eq!(
            fresh,
            vec![DesktopEvent::ProjectError {
                project_id: notes,
                project_name: "notes".into(),
                message: "notes broke (2)".into(),
            }]
        );
        assert!(fresh_project_errors(&mut seen, &workspace, api).is_empty());
        // Switching to `notes` doesn't replay an error already shown.
        assert!(fresh_project_errors(&mut seen, &workspace, notes).is_empty());
    }

    /// A service with an empty, private registry: the trust state only
    /// needs a handle, never the user's projects.
    fn service() -> forge_session::ProjectServiceHandle {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let registry = forge_session::ProjectRegistry::at(std::env::temp_dir().join(format!(
            "forge-desktop-trust-{}-{nanos}/projects.toml",
            std::process::id()
        )));
        let mut options = forge_session::ProjectServiceOptions::user().unwrap();
        options.registry = registry;
        forge_session::ProjectService::spawn(options).unwrap()
    }

    /// The page trusts a workspace before it subscribes, so the answer has to
    /// flip exactly when the pending decision is taken.
    #[tokio::test]
    async fn the_trust_screen_lasts_until_the_pending_decision_is_taken() {
        let projects = service();
        let ready = TrustState::ready(projects.clone(), "/work/repo".into());
        assert_eq!(trust_view(&ready).workspace, "/work/repo");
        assert!(!trust_view(&ready).needs_trust);

        let id = forge_session::ProjectId::new();
        let pending =
            TrustState::pending(projects.clone(), "/work/repo".into(), Pending::Trust(id));
        let view = trust_view(&pending);
        assert!(view.needs_trust && !view.needs_confirmation);
        assert_eq!(pending.take_pending(), Some(Pending::Trust(id)));
        assert!(!trust_view(&pending).needs_trust);
        assert_eq!(pending.fail("cannot start".into()), "cannot start");
        assert_eq!(trust_view(&pending).error, "cannot start");

        // An unregistered plain folder asks to be added on the same screen.
        let confirm = TrustState::pending(
            projects,
            "/work/notes".into(),
            Pending::Confirm("/work/notes".into()),
        );
        let view = trust_view(&confirm);
        assert!(view.needs_trust && view.needs_confirmation);
    }

    /// A launch with no folder shows the picker until one is chosen; a chosen
    /// folder that is untrusted then shows the trust screen.
    #[tokio::test]
    async fn the_folder_picker_lasts_until_a_folder_is_chosen() {
        let projects = service();
        let state = TrustState::needs_folder(projects.clone());
        let view = trust_view(&state);
        assert!(view.needs_folder && !view.needs_trust && view.workspace.is_empty());

        state.set_workspace("/work/notes".into());
        state.restore(Pending::Trust(forge_session::ProjectId::new()));
        let view = trust_view(&state);
        assert!(!view.needs_folder && view.needs_trust);
        assert_eq!(view.workspace, "/work/notes");
        assert!(!trust_view(&TrustState::ready(projects, "/work/repo".into())).needs_folder);
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
