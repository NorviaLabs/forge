//! Full-screen TUI application (TUI-01 shell + TUI-02/03/04 wired).

use std::collections::HashSet;
use std::fs;
use std::io::{self, stdout};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{
    self, EnableBracketedPaste, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
use forge_connect::{
    builtin_registry, handle_connect_action, models_for_picker, needs_tui_api_key_prompt,
    needs_tui_oauth, normalize_model_id, ConnectAction, ConnectError, ConnectRegistry,
    ConnectService, CredentialStore, ModelCatalogCache, OauthPending, OPENAI_CODEX_PROFILE_ID,
};
use forge_core::{AgentSession, ApplyOutcome, LoopError};
use forge_tools::{GitTool, Tool, ToolContext};
use forge_types::{
    HitlDecision, HitlPayload, Message, MessageRole, ModelStreamEvent, ProgressDocument,
};
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::backend::CrosstermBackend;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use ratatui::Terminal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

use crate::activity::{ActivityFeed, ActivityKind};
use crate::commands::{parse_slash, SlashCommand};
use crate::composer_layout::ComposerLayoutCache;
use crate::conversation::{
    format_elapsed_tenths, BannerKind, ChatItem, ConversationModel, ConversationViewOpts,
    StreamWaitPhase,
};
use crate::editor::EditorError;
use crate::effort::ReasoningEffort;
use crate::file_explorer::{FileExplorer, FileExplorerWidget, FileKind};
use crate::file_ops::{
    DeleteMode, EntryKind, FileOperationError, FileOperationKind, WorkspaceFileOps,
};
use crate::git_status::GitStatusKind;
use crate::history::InputHistory;
use crate::layout::is_too_small;
#[cfg(test)]
use crate::layout::split_areas_full;
use crate::layout::split_areas_with_chrome;
use crate::msg_queue::MessageQueue;
use crate::overlays::{
    centered_rect, filter_palette, handle_overlay_key, models_from_catalog, ApprovalExecutionMode,
    ApprovalOverlayState, ConnectProfileItem, FileExplorerItem, Key, Key as OverlayKey, Overlay,
    OverlayAction, OverlayWidget, PaletteItem, ResumeSessionItem,
};
use crate::run::{RunExecutionMode, RunHistoryFile, RunState, RunStateModel};
use crate::sidebar::{InspectorView, SidebarModel, SidebarWidget};
use crate::source_viewer::{SourceViewer, SourceViewerWidget};
use crate::terminal::TerminalGuard;
use crate::theme;
use crate::user_message_gutter::{gutter_glyph, gutter_prefix_width};
use crate::widgets::{
    classify_operator_error, BottomPanel, BottomPanelModel, BottomPanelState, BottomPanelTab,
    BusyPhase, FeedbackBar, FeedbackModel, FeedbackSeverity, FooterBar, FooterModel, InputBar,
    InputModel, StatusBar, StatusModel,
};
use forge_config::{CommandConfig, FileIconMode};

use crate::{MAX_RECENT_RUNS, RUN_HISTORY_VERSION};

mod approvals;
// `TuiApp` holds a set of these and the overlay renderer reads their labels,
// so the type is named here even though it lives with the approval logic.
use approvals::ApprovalIdentity;
mod chrome;
mod commands;
mod connect;
mod context;
mod files;
mod focus;
mod input;
mod mouse;
mod overlays;
mod persist;
/// `TuiApp::draw` lives in `app/render.rs`. Rust allows inherent `impl` blocks
/// for a type across several modules of the same crate, so this is a file split
/// only — `TuiApp`'s fields and every signature are unchanged.
mod render;
mod run;
mod shell;
mod turn;
mod util;
mod watch;
mod workspace;

pub use shell::run_tui;

const WORKSPACE_HISTORY_LIMIT: usize = 32;
const UI_STATE_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkspaceView {
    Conversation,
    File(PathBuf),
    Diff(DiffCommandContext),
    Run(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceViewKind {
    Conversation,
    File,
    Diff,
    Run,
}

impl WorkspaceView {
    fn kind(&self) -> WorkspaceViewKind {
        match self {
            Self::Conversation => WorkspaceViewKind::Conversation,
            Self::File(_) => WorkspaceViewKind::File,
            Self::Diff(_) => WorkspaceViewKind::Diff,
            Self::Run(_) => WorkspaceViewKind::Run,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceNavigation {
    current: WorkspaceView,
    history: Vec<WorkspaceView>,
}

impl Default for WorkspaceNavigation {
    fn default() -> Self {
        Self {
            current: WorkspaceView::Conversation,
            history: Vec::new(),
        }
    }
}

impl WorkspaceNavigation {
    fn push_view(&mut self, view: WorkspaceView) {
        if self.current == view {
            return;
        }
        self.history.push(self.current.clone());
        if self.history.len() > WORKSPACE_HISTORY_LIMIT {
            let overflow = self.history.len() - WORKSPACE_HISTORY_LIMIT;
            self.history.drain(0..overflow);
        }
        self.current = view;
    }

    fn replace_view(&mut self, view: WorkspaceView) {
        self.current = view;
    }

    fn navigate_to(&mut self, view: WorkspaceView) {
        if self.current.kind() == view.kind() {
            self.replace_view(view);
        } else {
            self.push_view(view);
        }
    }

    fn home(&mut self) {
        self.history.clear();
        self.current = WorkspaceView::Conversation;
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
enum FilesVisibility {
    Open,
    #[default]
    Closed,
}

impl FilesVisibility {
    fn from_open(open: bool) -> Self {
        if open {
            Self::Open
        } else {
            Self::Closed
        }
    }

    fn is_open(self) -> bool {
        matches!(self, Self::Open)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RepositoryUiState {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    repository_or_workspace_id: String,
    #[serde(default)]
    files_visibility: FilesVisibility,
    #[serde(default)]
    theme: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct TerminalCapture {
    title: Option<String>,
    content: String,
    truncated: bool,
}

#[derive(Debug, Clone)]
struct FileChangeEvent {
    path: PathBuf,
}

#[derive(Debug)]
enum RunEvent {
    Output(Vec<u8>),
    Finished {
        exit_code: Option<i32>,
        success: bool,
    },
    SpawnFailed(String),
    CaptureFailed(String),
}

/// The spatially stable keyboard regions.  This is intentionally small:
/// component-specific selection state remains with the component itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FocusBlock {
    Files,
    Workspace,
    Composer,
    Inspector,
    BottomPanel,
}

impl FocusBlock {
    fn label(self) -> &'static str {
        match self {
            Self::Files => "FILES",
            Self::Workspace => "CHAT",
            Self::Composer => "COMPOSER",
            Self::Inspector => "INSPECTOR",
            Self::BottomPanel => "PANEL",
        }
    }
}

impl FocusBlock {
    const ORDER: [Self; 5] = [
        Self::Files,
        Self::Workspace,
        Self::Composer,
        Self::Inspector,
        Self::BottomPanel,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransientOwner {
    SourceSearch,
    JumpToLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusMode {
    Navigation,
    Transient(TransientOwner),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExplorerNameAction {
    CreateFile,
    CreateDirectory,
    Rename,
}

#[derive(Debug, Clone)]
enum ExplorerDialog {
    Name {
        action: ExplorerNameAction,
        parent: PathBuf,
        source: Option<PathBuf>,
        input: String,
        error: Option<String>,
    },
    ConfirmCreate {
        action: ExplorerNameAction,
        parent: PathBuf,
        name: String,
        path: PathBuf,
    },
    ConfirmRename {
        source: PathBuf,
        path: PathBuf,
        name: String,
    },
    ConfirmDelete {
        source: PathBuf,
        name: String,
        kind: EntryKind,
        non_empty: bool,
        permanent: bool,
        error: Option<String>,
    },
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum DiffCommandContext {
    Current,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum RunCommandTarget {
    Current,
    Id(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActivitySummaryAction {
    OpenRun(String),
    ReviewChanges,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActivitySummaryModel {
    label: String,
    action_label: Option<&'static str>,
    action: Option<ActivitySummaryAction>,
    kind: BannerKind,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlashCommandOrigin {
    Composer,
    GlobalPalette,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum SemanticCommand {
    GoHome,
    GoBack,
    PushView(WorkspaceView),
    ReplaceView(WorkspaceView),
    OpenFile(PathBuf),
    ReviewChanges(DiffCommandContext),
    OpenRun(RunCommandTarget),
    ToggleFiles,
    CloseOverlay,
    FocusComposer,
    FocusPane(FocusBlock),
    SubmitMessage,
    InsertComposerNewline,
    OpenSlashCommands,
    OpenGlobalCommandPalette,
    OpenHelp,
    ActivateActivitySummary,
    SelectEntry(PathBuf),
    MoveFileSelection(isize),
    ExpandSelectedDirectory,
    CollapseSelectedDirectory,
    ToggleDirectory(PathBuf),
    OpenSelectedEntry,
    CancelCurrentInteraction,
    ConfirmCurrentInteraction,
    DispatchSlash {
        origin: SlashCommandOrigin,
        line: String,
    },
    CycleFocus {
        forward: bool,
    },
    ToggleInspector,
    CycleInspectorTab {
        forward: bool,
    },
    ToggleBottomPanel,
    CycleBottomPanelTab {
        forward: bool,
    },
    OpenBottomPanel(BottomPanelTab),
    RefreshFiles,
    RefreshEditor,
    RefreshDiff,
    BeginCreateFile,
    BeginCreateDirectory,
    BeginRename,
    RequestDelete,
    SelectPreviousChange,
    SelectNextChange,
    StartSourceSearch,
    StartJumpToLine,
    OpenExternalEditor,
    ToggleCurrentFileAttachment,
    ToggleToolDetails,
    MoveQueueSelection(i32),
    CancelSelectedQueueMessage,
    QuitOrInterrupt,
    Quit,
    RunOrCancel,
    Rerun,
    EditAndRerun,
    ToggleRunExecutionMode,
    EditRunCommand,
    EditRunDirectory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HitTarget {
    Pane(FocusBlock),
    FileEntry(PathBuf),
    DirectoryChevron(PathBuf),
    ActivitySummary,
    VisibleControl(SemanticCommand),
    Composer,
    OverlayAction(OverlayAction),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HitRegion {
    area: ratatui::layout::Rect,
    target: HitTarget,
    generation: u64,
    z_order: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DoubleClickTarget {
    FileEntry(PathBuf),
}

#[derive(Debug, Clone)]
struct PendingDoubleClick {
    target: DoubleClickTarget,
    button: MouseButton,
    timestamp: Instant,
    frame_generation: u64,
}

const DOUBLE_CLICK_THRESHOLD: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DiffSnapshot {
    paths: Vec<PathBuf>,
    stale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabNavCommand {
    PreviousTab,
    NextTab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FocusState {
    block: FocusBlock,
    mode: FocusMode,
    previous_block: Option<FocusBlock>,
    return_block: Option<FocusBlock>,
}

impl Default for FocusState {
    fn default() -> Self {
        Self {
            block: FocusBlock::Composer,
            mode: FocusMode::Navigation,
            previous_block: None,
            return_block: Some(FocusBlock::Workspace),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FocusAvailability {
    files: bool,
    inspector: bool,
    bottom_panel: bool,
}

impl FocusAvailability {
    fn contains(self, block: FocusBlock) -> bool {
        match block {
            FocusBlock::Files => self.files,
            FocusBlock::Workspace => true,
            FocusBlock::Composer => true,
            FocusBlock::Inspector => self.inspector,
            FocusBlock::BottomPanel => self.bottom_panel,
        }
    }
}
use crate::ExitCode;

#[derive(Debug, Error)]
pub enum TuiError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Loop(#[from] LoopError),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct ExitSummary {
    pub exit_code: ExitCode,
    pub session_id: String,
    pub token_usage: Option<String>,
}

fn failure_category_label(category: &str) -> String {
    match category {
        "validation_exhausted" => "Tool retries exhausted".into(),
        "no_final_answer" => "Turn incomplete".into(),
        "max_turns" => "Step limit reached".into(),
        other => {
            // Keep only short snake_case categories; never raw payloads.
            let cleaned = other.replace('_', " ");
            if cleaned.chars().count() <= 28
                && cleaned
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == ' ')
            {
                let mut chars = cleaned.chars();
                match chars.next() {
                    Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
                    None => "Failed".into(),
                }
            } else {
                "Failed".into()
            }
        }
    }
}

fn format_exit_token_usage(report: &forge_core::TokenUsageReport) -> String {
    let api = &report.api;
    format!(
        "Token usage: total={} input={} (+ {} cached) output={} (reasoning {})",
        format_with_commas(api.total_api_tokens()),
        format_with_commas(api.prompt_tokens),
        format_with_commas(api.prompt_cache_hits),
        format_with_commas(api.completion_tokens),
        format_with_commas(api.thinking_tokens_est),
    )
}

fn format_with_commas(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

#[derive(Debug, Clone)]
pub struct TuiRuntimeConfig {
    pub model_label: String,
    pub provider: String,
    pub cwd: PathBuf,
    pub version: String,
    pub startup_notices: Vec<String>,
    pub validation_command: Option<CommandConfig>,
    pub file_icons: FileIconMode,
    pub mouse_capture: bool,
    pub theme: forge_config::Theme,
}

impl Default for TuiRuntimeConfig {
    fn default() -> Self {
        Self {
            model_label: String::new(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "test".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::default(),
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResumeSession {
    id: uuid::Uuid,
    modified: SystemTime,
}

fn recent_resume_sessions(
    dir: &std::path::Path,
    current: uuid::Uuid,
    limit: usize,
) -> io::Result<Vec<ResumeSession>> {
    let mut sessions = Vec::new();
    if !dir.is_dir() {
        return Ok(sessions);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("db") {
            continue;
        }
        let Some(id) = path
            .file_stem()
            .and_then(|value| value.to_str())
            .and_then(|value| uuid::Uuid::parse_str(value).ok())
        else {
            continue;
        };
        if id == current {
            continue;
        }
        let modified = entry
            .metadata()?
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        sessions.push(ResumeSession { id, modified });
    }
    sessions.sort_by_key(|session| std::cmp::Reverse(session.modified));
    sessions.truncate(limit);
    Ok(sessions)
}

fn footer_usage_summary_with_cost(
    report: &forge_core::TokenUsageReport,
    cost: Option<forge_connect::CatalogCost>,
) -> String {
    let cost = cost
        .filter(|_| report.api.prompt_tokens > 0 || report.api.completion_tokens > 0)
        .map(|cost| {
            let input = report.api.prompt_tokens as f64 * cost.input / 1_000_000.0;
            let output = report.api.completion_tokens as f64 * cost.output / 1_000_000.0;
            format!(" · ${:.4}", input + output)
        })
        .unwrap_or_default();
    format!(
        "in {} · out {} · total {}{}",
        format_with_commas(report.api.prompt_tokens),
        format_with_commas(report.api.completion_tokens),
        format_with_commas(report.api.total_api_tokens()),
        cost,
    )
}

#[derive(Debug, Clone, Default)]
struct FooterLimits {
    usage: String,
    weekly_limit: String,
    credits: String,
}

fn footer_limits_from_report(lines: &[String]) -> FooterLimits {
    FooterLimits {
        usage: lines
            .iter()
            .find(|line| line.starts_with("Session limit:"))
            .cloned()
            .unwrap_or_default(),
        weekly_limit: lines
            .iter()
            .find(|line| line.starts_with("Weekly limit:"))
            .cloned()
            .unwrap_or_default(),
        credits: lines
            .iter()
            .find(|line| line.starts_with("Credits:") || line.starts_with("Credit balance:"))
            .cloned()
            .unwrap_or_default(),
    }
}

#[allow(dead_code)]
fn footer_usage_summary(
    report: &forge_core::TokenUsageReport,
    cost: Option<forge_connect::CatalogCost>,
    limits: &FooterLimits,
) -> crate::widgets::FooterModel {
    crate::widgets::FooterModel {
        usage: limits.usage.clone(),
        weekly_limit: limits.weekly_limit.clone(),
        credits: limits.credits.clone(),
        usage_summary: footer_usage_summary_with_cost(report, cost),
        ..Default::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConversationRenderKey {
    session_id: uuid::Uuid,
    width: u16,
    messages: usize,
    last_message_content: usize,
    last_message_thinking: usize,
    events: usize,
    last_event_detail: usize,
    banners: usize,
    queue: usize,
    queue_selected: Option<usize>,
    chat_message_start: usize,
    chat_event_start: usize,
    busy: bool,
    busy_phase: String,
    activity_summary: Option<(String, Option<&'static str>, BannerKind)>,
    tool_expanded: bool,
    splash_dismissed: bool,
    slash_mode: bool,
    status: forge_types::SessionStatus,
    theme: forge_config::Theme,
}

struct ConversationRenderCache {
    key: ConversationRenderKey,
    /// Shared so the render path can hold the lines without copying them. A
    /// frame clones the handle, not the ~940KB of `Line`/`Span` data behind it.
    lines: Arc<Vec<Line<'static>>>,
}

#[allow(dead_code)]
struct FooterLimitsCache {
    provider: String,
    fetched_at: Instant,
    limits: FooterLimits,
}

pub struct TuiApp {
    pub session: AgentSession,
    pub input: InputModel,
    pub overlay: Option<Overlay>,
    pub should_quit: bool,
    pub busy: bool,
    pub status_message: String,
    pub runtime: TuiRuntimeConfig,
    pub last_exit: ExitCode,
    pub connect_registry: ConnectRegistry,
    pub connect_store: CredentialStore,
    pub connect_profile: Option<String>,
    /// Manual disconnect latch: prevents auto-restore until the user signs in again.
    pub auth_suspended: bool,
    /// In-flight xAI device-code OAuth (polled on the event loop tick).
    pub oauth_pending: Option<OauthPending>,
    /// Last time we polled the token endpoint (respect server `interval`).
    oauth_last_poll: Option<std::time::Instant>,
    /// Phase 7 — submitted command history (Up/Down when no overlay).
    pub history: InputHistory,
    /// Phase 8 autocomplete: selection within filtered `/` suggestions.
    pub slash_suggest_idx: usize,
    /// Multi-line notices (e.g. /connect list) shown above the input.
    pub notices: Vec<String>,
    notices_until: Option<Instant>,
    /// Phase 10 / TUI-08 — always-visible feedback strip model.
    pub feedback: FeedbackModel,
    /// Phase 10 / TUI-08 — durable UI error/info banners in chat.
    pub ui_banners: Vec<ChatItem>,
    /// Phase 10 / TUI-10 — progressive busy phase for chrome.
    pub busy_phase: BusyPhase,
    /// User prompt queued on Enter; drained by the event loop so the YOU bubble paints first.
    pending_prompt: Option<String>,
    /// Resume the current agent loop after an interactive turn-limit checkpoint.
    pending_turn_continue: bool,
    /// Long-running slash action queued to run on the event loop (so the command echo paints).
    pending_sync: bool,
    /// HITL resolve queued to run on the event loop (journals + state updates).
    pending_hitl_decision: Option<HitlDecision>,
    /// Context reset queued to run on the event loop.
    pending_context_reset: bool,
    /// External-editor request queued for the event loop (terminal suspend/resume).
    pending_external_editor: bool,
    pending_validation: bool,
    /// Active-file context attachment for the next user message.
    pending_attachment: Option<crate::file_context::FileAttachment>,
    /// Additional user messages waiting to run after the current turn (FIFO).
    message_queue: MessageQueue,
    /// Selected queued row for keyboard cancellation.
    queue_selected: Option<usize>,
    /// Live assistant text while tokens stream in.
    stream_preview: String,
    /// Live thinking/reasoning text while tokens stream in.
    stream_thinking: String,
    /// When the current model turn started (for wait/think elapsed timer).
    turn_started: Option<Instant>,
    /// When the first thinking token arrived.
    thinking_started: Option<Instant>,
    /// Duration of the thinking phase once it ends (used for persistence/telemetry).
    thought_secs: Option<f64>,
    /// Optional web_search label for chrome (`mock` / `off` / provider id).
    pub web_search_label: Option<String>,
    /// Phase 10 / TUI-10 — activity ring buffer.
    pub activity: ActivityFeed,
    /// Reasoning effort sent to model providers (`auto` omits the parameter).
    reasoning_effort: ReasoningEffort,
    /// Expand last tool detail (Ctrl+O).
    tool_expanded: bool,
    /// V3.1 contextual workspace navigation.
    workspace_navigation: WorkspaceNavigation,
    /// Read-only source viewer state for the File workspace view.
    pub source_viewer: SourceViewer,
    file_watcher: Option<RecommendedWatcher>,
    file_change_rx: Receiver<FileChangeEvent>,
    file_change_tx: Sender<FileChangeEvent>,
    pub bottom_panel: BottomPanelState,
    pub run: RunStateModel,
    pub files_visible: bool,
    pub file_explorer: FileExplorer,
    explorer_dialog: Option<ExplorerDialog>,
    /// Authoritative keyboard ownership. Legacy component `focused` flags are
    /// synchronised from this state for rendering only.
    focus: FocusState,
    /// User preference; narrow terminals still hide the sidebar responsively.
    sidebar_visible: bool,
    inspector_view: InspectorView,
    /// Selected index in the changed-files inventory for Diff workspace.
    diff_selected: usize,
    /// Soft-cancel in-flight turn (Esc while busy).
    cancel_requested: bool,
    /// Exact Direct invocations remembered for this Forge session only.
    hitl_session_allow: HashSet<ApprovalIdentity>,
    /// Transient toast (auto-clears).
    toast: Option<(Instant, String)>,
    /// Last measured height of the editor viewport for page scrolling.
    last_editor_height: u16,
    /// Session message/event offsets hidden by the most recent `/clear`.
    chat_message_start: usize,
    chat_event_start: usize,
    /// Conversation scroll offset (when not following).
    chat_scroll: u16,
    chat_follow: bool,
    context_reset_snapshot: Option<(f64, f64)>,
    splash_dismissed: bool,
    conversation_cache: Option<ConversationRenderCache>,
    composer_layout_cache: ComposerLayoutCache,
    model_cost_cache: Option<(String, Option<forge_connect::CatalogCost>)>,
    footer_limits_cache: Option<FooterLimitsCache>,
    footer_limits_rx: Option<std::sync::mpsc::Receiver<(String, FooterLimits)>>,
    /// Last known repo header. Refreshed off-thread by `poll_repo_header`; the
    /// render path only ever reads it, never derives it.
    repo_header: RepoHeaderCache,
    repo_header_rx: Option<std::sync::mpsc::Receiver<RepoHeaderCache>>,
    repo_header_refreshed_at: Instant,
    /// Directory the cached header describes, so a cwd change invalidates it.
    repo_header_cwd: PathBuf,
    terminal_capture: TerminalCapture,
    hit_regions: Vec<HitRegion>,
    frame_generation: u64,
    pending_double_click: Option<PendingDoubleClick>,
    diff_snapshot: DiffSnapshot,
    run_rx: Option<std::sync::mpsc::Receiver<RunEvent>>,
    run_abort: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Debug, Clone)]
pub(crate) struct RepoHeaderCache {
    pub(crate) repo_name: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) dirty: bool,
}

impl TuiApp {
    pub fn new(session: AgentSession, runtime: TuiRuntimeConfig) -> Self {
        crate::theme::set_active(runtime.theme);
        let mut input = InputModel::default();
        input.hint = "Describe a task…".into();
        let startup_notices = runtime.startup_notices.clone();
        let file_icons = runtime.file_icons;
        let workspace_root = session.workspace_root().to_path_buf();
        let run = RunStateModel::new(workspace_root.clone(), runtime.validation_command.clone());
        let (file_change_tx, file_change_rx) = mpsc::channel();
        // One synchronous read at startup so the first frame shows the real branch
        // instead of blanking until the first background refresh lands.
        let repo_header_cwd = runtime.cwd.clone();
        let repo_header = chrome::load_repo_header(&repo_header_cwd);
        let mut app = Self {
            session,
            input,
            overlay: None,
            should_quit: false,
            busy: false,
            status_message: String::new(),
            runtime,
            last_exit: ExitCode::Success,
            connect_registry: builtin_registry(),
            connect_store: CredentialStore::user_default(),
            connect_profile: None,
            auth_suspended: false,
            oauth_pending: None,
            oauth_last_poll: None,
            history: InputHistory::default(),
            slash_suggest_idx: 0,
            notices: startup_notices,
            notices_until: None,
            feedback: FeedbackModel::default(),
            ui_banners: Vec::new(),
            busy_phase: BusyPhase::Idle,
            web_search_label: Some("mock".into()),
            activity: ActivityFeed::default(),
            pending_prompt: None,
            pending_turn_continue: false,
            pending_sync: false,
            pending_hitl_decision: None,
            pending_context_reset: false,
            pending_external_editor: false,
            pending_validation: false,
            pending_attachment: None,
            message_queue: MessageQueue::new(),
            queue_selected: None,
            stream_preview: String::new(),
            stream_thinking: String::new(),
            turn_started: None,
            thinking_started: None,
            thought_secs: None,
            reasoning_effort: ReasoningEffort::Auto,
            tool_expanded: false,
            workspace_navigation: WorkspaceNavigation::default(),
            source_viewer: SourceViewer::new(),
            file_watcher: None,
            file_change_rx,
            file_change_tx,
            bottom_panel: BottomPanelState::default(),
            run,
            files_visible: false,
            file_explorer: FileExplorer::new(Some(workspace_root), file_icons),
            explorer_dialog: None,
            focus: FocusState::default(),
            sidebar_visible: false,
            inspector_view: InspectorView::default(),
            diff_selected: 0,
            cancel_requested: false,
            hitl_session_allow: HashSet::new(),
            toast: None,
            chat_message_start: 0,
            chat_event_start: 0,
            chat_scroll: 0,
            chat_follow: true,
            context_reset_snapshot: None,
            splash_dismissed: false,
            conversation_cache: None,
            composer_layout_cache: ComposerLayoutCache::default(),
            model_cost_cache: None,
            footer_limits_cache: None,
            footer_limits_rx: None,
            repo_header,
            repo_header_rx: None,
            repo_header_refreshed_at: Instant::now(),
            repo_header_cwd: repo_header_cwd.clone(),
            terminal_capture: TerminalCapture::default(),
            hit_regions: Vec::new(),
            frame_generation: 0,
            pending_double_click: None,
            diff_snapshot: DiffSnapshot::default(),
            run_rx: None,
            run_abort: None,
            last_editor_height: 24,
        };
        app.init_file_watcher();
        app.load_run_history();
        app.load_ui_state();
        app.normalize_restored_run();
        app.restore_saved_auth().apply_connection_chrome()
    }
}

#[cfg(test)]
mod tests;
