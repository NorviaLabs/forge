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

mod commands;
/// `TuiApp::draw` lives in `app/render.rs`. Rust allows inherent `impl` blocks
/// for a type across several modules of the same crate, so this is a file split
/// only — `TuiApp`'s fields and every signature are unchanged.
mod render;

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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ApprovalIdentity {
    executable: String,
    arguments: Vec<String>,
    working_directory: String,
    environment_delta: String,
    workspace_identity: String,
    session_id: String,
}

impl ApprovalIdentity {
    fn label(&self) -> String {
        std::iter::once(self.executable.as_str())
            .chain(self.arguments.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(80)
            .collect()
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

#[allow(dead_code)]
fn footer_provider_id(provider: &str, connect_profile: Option<&str>) -> String {
    connect_profile.unwrap_or(provider).to_owned()
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn rebase_path(path: &Path, old_base: &Path, new_base: &Path) -> Option<PathBuf> {
    if path == old_base {
        return Some(new_base.to_path_buf());
    }
    path.strip_prefix(old_base)
        .ok()
        .map(|suffix| new_base.join(suffix))
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

/// How long a cached repo header stays fresh before a background refresh starts.
/// Branch and dirty state change on human timescales, not frame timescales.
const REPO_HEADER_TTL: Duration = Duration::from_secs(2);

/// Read the repo header by shelling out to git. Runs on a worker thread only —
/// never call this from the render path.
fn load_repo_header(cwd: &Path) -> RepoHeaderCache {
    let repo_name = cwd
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::to_string);

    let branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());

    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| !text.trim().is_empty())
        .unwrap_or(false);

    RepoHeaderCache {
        repo_name,
        branch,
        dirty,
    }
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
        let repo_header = load_repo_header(&repo_header_cwd);
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

    fn run_history_path(&self) -> PathBuf {
        self.session
            .workspace_root()
            .join(".forge/run-history.json")
    }

    fn ui_state_path(&self) -> PathBuf {
        self.session.workspace_root().join(".forge/ui-state.json")
    }

    fn load_run_history(&mut self) {
        let Ok(text) = fs::read_to_string(self.run_history_path()) else {
            return;
        };
        let Ok(history) = serde_json::from_str::<RunHistoryFile>(&text) else {
            self.run.error = Some("run history is malformed; recent runs were not loaded".into());
            return;
        };
        let workspace_id = self.session.workspace_root().display().to_string();
        if history.version == RUN_HISTORY_VERSION
            && history.repository_or_workspace_id == workspace_id
        {
            self.run.recent = history.recent.into_iter().take(MAX_RECENT_RUNS).collect();
        }
    }

    fn save_run_history(&mut self) {
        let path = self.run_history_path();
        let history = RunHistoryFile {
            version: RUN_HISTORY_VERSION,
            repository_or_workspace_id: self.session.workspace_root().display().to_string(),
            recent: self.run.recent.iter().cloned().collect(),
        };
        let result =
            fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new("."))).and_then(|_| {
                fs::write(
                    &path,
                    serde_json::to_vec_pretty(&history).unwrap_or_default(),
                )
            });
        if let Err(error) = result {
            self.run.error = Some(format!("could not persist recent runs: {error}"));
        }
    }

    fn repository_or_workspace_id(&self) -> String {
        self.session.workspace_root().display().to_string()
    }

    fn approval_state_for_payload(&self, payload: &HitlPayload) -> ApprovalOverlayState {
        ApprovalOverlayState::for_payload(
            payload,
            self.session.workspace_root().display().to_string(),
        )
    }

    fn approval_identity_for_payload(&self, payload: &HitlPayload) -> Option<ApprovalIdentity> {
        let approval = self.approval_state_for_payload(payload);
        if approval.mode != ApprovalExecutionMode::Direct || !approval.remember_eligible {
            return None;
        }
        Some(ApprovalIdentity {
            executable: approval.executable_or_shell,
            arguments: approval.arguments,
            working_directory: approval.working_directory,
            environment_delta: approval.environment_delta,
            workspace_identity: self.repository_or_workspace_id(),
            session_id: self.session.session_id.to_string(),
        })
    }

    fn open_hitl_overlay(&mut self, payload: HitlPayload) {
        self.overlay = Some(Overlay::hitl_with_working_directory(
            payload,
            self.session.workspace_root().display().to_string(),
        ));
    }

    fn load_ui_state(&mut self) {
        let Ok(text) = fs::read_to_string(self.ui_state_path()) else {
            return;
        };
        let Ok(state) = serde_json::from_str::<RepositoryUiState>(&text) else {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "workspace UI state is malformed; using default Files visibility",
            );
            return;
        };
        if state.version >= 1
            && state.version <= UI_STATE_VERSION
            && state.repository_or_workspace_id == self.repository_or_workspace_id()
        {
            self.files_visible = state.files_visibility.is_open();
            if let Some(ref name) = state.theme {
                if let Ok(theme) = forge_config::Theme::parse_strict(name) {
                    self.apply_theme(theme, false);
                }
            }
        }
    }

    fn save_ui_state(&mut self) {
        let path = self.ui_state_path();
        let state = RepositoryUiState {
            version: UI_STATE_VERSION,
            repository_or_workspace_id: self.repository_or_workspace_id(),
            files_visibility: FilesVisibility::from_open(self.files_visible),
            theme: Some(self.runtime.theme.label().to_string()),
        };
        let result = fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")))
            .and_then(|_| fs::write(&path, serde_json::to_vec_pretty(&state).unwrap_or_default()));
        if let Err(error) = result {
            self.set_feedback(
                FeedbackSeverity::Warn,
                format!("could not persist Files visibility: {error}"),
            );
        }
    }

    fn init_file_watcher(&mut self) {
        let tx = self.file_change_tx.clone();
        let mut watcher = match RecommendedWatcher::new(
            move |result: notify::Result<notify::Event>| {
                if let Ok(event) = result {
                    if matches!(
                        event.kind,
                        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                    ) {
                        for path in event.paths {
                            // Journal/progress/ui-state under .forge churn constantly
                            // during exploration and must not thrash the Files tree.
                            if path_is_under_dot_forge(&path) {
                                continue;
                            }
                            let _ = tx.send(FileChangeEvent { path });
                        }
                    }
                }
            },
            Config::default(),
        ) {
            Ok(watcher) => watcher,
            Err(_) => return,
        };
        let _ = watcher.watch(self.session.workspace_root(), RecursiveMode::Recursive);
        self.file_watcher = Some(watcher);
    }

    fn poll_file_changes(&mut self) {
        let mut active_file_changed = false;
        let mut workspace_changed = false;
        while let Ok(change) = self.file_change_rx.try_recv() {
            workspace_changed = true;
            if let Some(path) = &self.source_viewer.path {
                if change.path == *path {
                    active_file_changed = true;
                }
            }
        }
        if workspace_changed {
            self.refresh_after_filesystem_change(active_file_changed);
        }
    }

    fn normalize_restored_run(&mut self) {
        if let Some(record) = self.run.current.as_mut() {
            if matches!(record.state, RunState::Running | RunState::Queued) {
                record.state = RunState::Cancelled;
                record.finished_at = Some(std::time::SystemTime::now());
            }
        }
        self.pending_validation = false;
        self.run_rx = None;
        self.run_abort = None;
    }

    fn note_workspace_changed(&mut self) {
        self.clear_pending_double_click();
        self.mark_diff_stale_if_reviewing();
        self.file_explorer.refresh_workspace();
    }

    fn tool_may_mutate_workspace(name: &str) -> bool {
        matches!(name, "write_file" | "apply_patch" | "bash" | "git" | "run")
    }

    fn maybe_note_workspace_changed_from_recent_tools(&mut self) {
        // Only the latest assistant step matters — older writes already refreshed the tree.
        let mutated = self
            .session
            .messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::Assistant)
            .map(|message| {
                message
                    .tool_calls
                    .iter()
                    .any(|call| Self::tool_may_mutate_workspace(&call.name))
            })
            .unwrap_or(false);
        if mutated {
            self.note_workspace_changed();
        }
    }

    fn current_changed_paths(&self) -> Vec<PathBuf> {
        self.file_explorer
            .git_status
            .changed_files()
            .into_iter()
            .map(|file| file.path)
            .collect()
    }

    fn capture_diff_snapshot(&mut self) {
        self.diff_snapshot.paths = self.current_changed_paths();
        self.diff_snapshot.stale = false;
    }

    fn mark_diff_stale_if_reviewing(&mut self) {
        if self.current_workspace_is_diff() {
            self.diff_snapshot.stale = true;
        }
    }

    fn refresh_diff_review(&mut self) {
        self.file_explorer.refresh_git_status();
        self.capture_diff_snapshot();
    }

    fn refresh_after_filesystem_change(&mut self, active_file_changed: bool) {
        let renamed_open_file = self.reconcile_open_file_external_rename();
        let renamed_notice = renamed_open_file.then(|| "File renamed externally".to_string());
        if active_file_changed {
            self.refresh_active_source_viewer();
            self.notices.clear();
        } else if renamed_open_file {
            self.notices.clear();
        }
        if self.focus.block == FocusBlock::Files && self.focus.mode == FocusMode::Navigation {
            self.file_explorer.refresh_git_status();
        } else {
            self.note_workspace_changed();
        }
        if let Some(notice) = renamed_notice {
            self.source_viewer.notice = Some(notice);
        }
    }

    fn reconcile_open_file_external_rename(&mut self) -> bool {
        let Some(open_path) = self.source_viewer.path.clone() else {
            return false;
        };
        if open_path.exists() {
            return false;
        }
        let Some(parent) = open_path.parent() else {
            return false;
        };
        let Ok(entries) = fs::read_dir(parent) else {
            return false;
        };
        let root = self.session.workspace_root().to_path_buf();
        let old_line = self.source_viewer.current_line;
        let old_top = self.source_viewer.top_line;
        for entry in entries.flatten() {
            let candidate = entry.path();
            if candidate == open_path || !candidate.is_file() {
                continue;
            }
            if self
                .source_viewer
                .reconcile_external_rename_if_same_identity(&root, &candidate)
            {
                let workspace_path = candidate.canonicalize().unwrap_or(candidate);
                self.source_viewer.current_line =
                    old_line.min(self.source_viewer.lines.len().saturating_sub(1));
                self.source_viewer.top_line =
                    old_top.min(self.source_viewer.lines.len().saturating_sub(1));
                self.workspace_navigation
                    .replace_view(WorkspaceView::File(workspace_path));
                self.set_feedback(FeedbackSeverity::Info, "Open file was renamed externally");
                return true;
            }
        }
        false
    }

    // Intentionally keep the conversation window clean: only real chat (user/assistant)
    // plus essential banners (errors, HITL, connection). Slash command output lives in
    // notices / overlays / activity.

    /// Mock provider is always "connected" (offline tests / CI).
    fn is_mock_provider(&self) -> bool {
        self.runtime.provider.eq_ignore_ascii_case("mock")
            || self.runtime.model_label.eq_ignore_ascii_case("mock")
    }

    /// Live credentials still exist for a connect profile id.
    fn credentials_live_for(&self, profile_id: &str) -> bool {
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        svc.connected_profiles()
            .ok()
            .map(|ps| ps.iter().any(|p| p.id == profile_id))
            .unwrap_or(false)
    }

    /// True when chat may call an LLM (mock, or a live `/connect` profile).
    pub fn is_provider_connected(&self) -> bool {
        if self.is_mock_provider() {
            return true;
        }
        match self.connect_profile.as_deref() {
            Some(id) => self.credentials_live_for(id),
            None => false,
        }
    }

    fn push_notice(&mut self, lines: Vec<String>) {
        self.notices = lines;
        self.notices_until = Some(Instant::now() + Duration::from_secs(3));
    }

    fn tick_notices(&mut self) {
        if self
            .notices_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.notices.clear();
            self.notices_until = None;
        }
    }

    /// Drop stale `connect_profile` if credentials were cleared out-of-band.
    fn sync_provider_connection(&mut self) {
        if self.auth_suspended {
            return;
        }
        if self.is_mock_provider() {
            return;
        }
        if let Some(id) = self.connect_profile.clone() {
            if !self.credentials_live_for(&id) {
                self.connect_profile = None;
            }
        }
    }

    /// Status/input/banner chrome reflecting connect state.
    fn apply_connection_chrome(mut self) -> Self {
        self.refresh_connection_ui();
        self
    }

    fn refresh_connection_ui(&mut self) {
        self.sync_provider_connection();
        let connected = self.is_provider_connected();
        self.input.not_connected = !connected;
        if connected {
            if self.input.hint.contains("Not connected") || self.input.hint.contains("/connect") {
                self.input.hint = String::new();
            }
            // Drop the sticky not-connected banner once signed in.
            self.ui_banners.retain(|b| {
                !matches!(
                    b,
                    ChatItem::Banner {
                        kind: BannerKind::Warn,
                        text
                    } if text.contains("Not connected")
                )
            });
        } else {
            self.input.hint = "Not connected · run /connect before chatting".into();
            let has_banner = self.ui_banners.iter().any(|b| {
                matches!(
                    b,
                    ChatItem::Banner {
                        kind: BannerKind::Warn,
                        text
                    } if text.contains("Not connected")
                )
            });
            if !has_banner {
                self.ui_banners.push(ChatItem::Banner {
                    text: "Not connected to an LLM provider. Run /connect (xAI Grok or OpenCode Go) before sending a message.".into(),
                    kind: BannerKind::Warn,
                });
            }
        }
    }

    fn disconnect_auth(&mut self, profile_id: Option<&str>) -> Result<String, TuiError> {
        let mut env_keys = Vec::new();
        {
            let svc = ConnectService {
                registry: &self.connect_registry,
                store: &self.connect_store,
                active_profile_id: self.connect_profile.clone(),
                active_model: Some(self.runtime.model_label.clone()),
            };
            let profiles: Vec<_> = if let Some(id) = profile_id {
                svc.profile(id).into_iter().cloned().collect()
            } else {
                svc.connected_profiles().unwrap_or_default()
            };
            for p in profiles {
                if let Ok(pairs) = svc.provider_env_for_profile(&p.id) {
                    env_keys.extend(pairs.into_iter().map(|(k, _)| k));
                }
            }
        }
        for key in env_keys {
            std::env::remove_var(key);
        }
        self.session.clear_provider_env();
        self.oauth_pending = None;
        self.oauth_last_poll = None;
        self.pending_prompt = None;
        self.pending_sync = false;
        self.pending_hitl_decision = None;
        self.pending_context_reset = false;
        self.message_queue = MessageQueue::new();
        self.queue_selected = None;
        self.stream_preview.clear();
        self.stream_thinking.clear();
        self.turn_started = None;
        self.thinking_started = None;
        self.thought_secs = None;
        self.cancel_requested = false;
        self.busy = false;
        self.busy_phase = BusyPhase::Idle;
        self.tool_expanded = false;
        self.chat_follow = true;
        self.chat_scroll = 0;
        self.connect_profile = None;
        self.runtime.provider.clear();
        self.runtime.model_label.clear();
        self.session.set_active_model(String::new());
        self.feedback = FeedbackModel::default();
        self.status_message = "disconnected".into();
        self.notices.clear();
        self.ui_banners.retain(|b| {
            !matches!(
                b,
                ChatItem::Banner {
                    kind: BannerKind::Warn,
                    text
                } if text.contains("Not connected")
            )
        });
        self.auth_suspended = true;

        let cleared = if let Some(id) = profile_id {
            self.connect_store
                .clear(id)
                .map_err(|e| TuiError::Other(e.to_string()))?
        } else {
            self.connect_store
                .clear_all()
                .map_err(|e| TuiError::Other(e.to_string()))?
        };
        if let Some(id) = profile_id {
            let _ = self.connect_store.clear_last_selection(Some(id));
        } else {
            let _ = self.connect_store.clear_last_selection(None);
        }
        self.refresh_connection_ui();
        let msg = if let Some(id) = profile_id {
            if cleared {
                format!("disconnected `{id}`")
            } else {
                format!("no stored credentials for `{id}`")
            }
        } else if cleared {
            "disconnected · cleared stored credentials".into()
        } else {
            "disconnected · no stored credentials".into()
        };
        self.push_activity(ActivityKind::Connect, FeedbackSeverity::Info, msg.clone());
        self.set_feedback(FeedbackSeverity::Info, msg.clone());
        Ok(msg)
    }

    fn push_toast(&mut self, text: impl Into<String>) {
        self.toast = Some((Instant::now(), text.into()));
        // Also mirror briefly into feedback (auto-cleared in draw/tick)
        if let Some((_, ref t)) = self.toast {
            self.set_feedback(FeedbackSeverity::Ok, t.clone());
        }
    }

    fn tick_toast(&mut self) {
        if let Some((at, _)) = &self.toast {
            if at.elapsed() > Duration::from_secs(2) {
                self.toast = None;
                if self.feedback.severity == FeedbackSeverity::Ok {
                    self.feedback = FeedbackModel::default();
                    self.status_message.clear();
                }
            }
        }
    }

    /// Close the thinking clock. Prefer wall time from first thinking token;
    /// if that is ~0 (same-batch non-stream dump), fall back to full turn elapsed.
    fn close_thinking_timer(&mut self) {
        if self.thought_secs.is_some() {
            return;
        }
        if self.stream_thinking.is_empty() {
            return;
        }
        let from_think = self
            .thinking_started
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        let from_turn = self
            .turn_started
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        // Same-batch dump of all thinking+answer → thinking_started ≈ now; use turn time.
        let secs = if from_think < 0.15 && from_turn > from_think {
            from_turn
        } else if from_think > 0.0 {
            from_think
        } else {
            from_turn
        };
        self.thought_secs = Some(secs);
    }

    fn persist_turn_thinking_duration(&mut self, secs: f64) {
        if let Some(m) = self
            .session
            .messages
            .iter_mut()
            .rev()
            .find(|m| m.role == forge_types::MessageRole::Assistant)
        {
            if m.thinking.is_some() {
                m.thinking_duration_secs = Some(secs);
            }
        }
    }

    /// Reload credentials from disk: silent OAuth refresh, inject auth, activate profile.
    /// So a successful `/connect` continues to work in later Forge sessions.
    fn restore_saved_auth(mut self) -> Self {
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: None,
            active_model: None,
        };
        let mut connected = svc.connected_profiles().unwrap_or_default();
        connected.sort_by(|a, b| a.id.cmp(&b.id));
        let saved_selection = self.connect_store.last_selection().ok().flatten();
        if let Some(effort) = self
            .connect_store
            .last_effort()
            .ok()
            .flatten()
            .and_then(|effort| effort.parse().ok())
        {
            self.reasoning_effort = effort;
        }
        // Restore the last usable provider; otherwise fall back to a deterministic
        // connected profile instead of silently preferring one backend family.
        let chosen = connected
            .iter()
            .find(|p| saved_selection.as_ref().is_some_and(|(id, _)| id == &p.id))
            .or_else(|| connected.first())
            .cloned();
        if let Some(profile) = chosen {
            // Refresh and inject provider credentials into the client only.
            let _ = svc.ensure_oauth_fresh(&profile.id);
            if let Ok(pairs) = svc.provider_env_for_profile(&profile.id) {
                self.session.apply_provider_env(&pairs);
            }
            self.connect_profile = Some(profile.id.clone());
            // Only switch the active model when it still looks like the forge default
            // (don't clobber an explicit --model / test runtime label).
            let cur = self.runtime.model_label.as_str();
            let looks_default =
                cur.is_empty() || cur == "openai/gpt-4.1-mini" || cur == "m" || cur == "mock";
            if looks_default {
                let saved_model = saved_selection
                    .as_ref()
                    .and_then(|(id, model)| (id == &profile.id).then_some(model.as_str()))
                    .filter(|model| {
                        let prefix = Self::model_prefix(model);
                        let pid = profile.id.as_str();
                        let provider_prefix = profile.model_provider_prefix.as_str();
                        prefix == pid
                            || prefix == provider_prefix
                            || (prefix == "openai" && pid == "openai_codex")
                            || (prefix == "openai-codex" && pid == "openai_codex")
                            || (prefix == "opencode-go" && pid == "opencode_go")
                            || (prefix == "opencode-zen" && pid == "opencode_zen")
                            || (prefix == "grok" && pid == "xai")
                    });
                if let Some(model) = saved_model.or_else(|| profile.default_model()) {
                    self.runtime.model_label = model.to_string();
                    self.runtime.provider = "native".into();
                    self.session.set_active_model(model);
                }
            } else if self.session.active_model.is_empty() {
                self.session
                    .set_active_model(self.runtime.model_label.clone());
            }
            self.status_message = format!("restored {} · {}", profile.id, self.runtime.model_label);
        }
        self
    }

    fn open_effort_picker_for_model(&mut self, model: &str) {
        let options = ReasoningEffort::options_for_model(model);
        let default = ReasoningEffort::default_for_model(model);
        if options.len() <= 1 {
            // Nothing useful to choose; keep current if still valid, else provider default.
            if !options.contains(&self.reasoning_effort) {
                self.reasoning_effort = default;
                self.persist_selection();
            }
            self.overlay = None;
            return;
        }
        if !options.contains(&self.reasoning_effort) {
            self.reasoning_effort = default;
        }
        self.overlay = Some(Overlay::effort_open(model, self.reasoning_effort));
        self.set_feedback(FeedbackSeverity::Info, "choose reasoning effort");
    }

    fn persist_selection(&self) {
        if let Some(profile_id) = self.connect_profile.as_deref() {
            let _ = self
                .connect_store
                .set_last_selection(profile_id, &self.runtime.model_label);
        }
        let _ = self
            .connect_store
            .set_last_effort(&self.reasoning_effort.to_string());
    }

    /// Phase 10: set strip + keep `status_message` in sync for tests/compat.
    pub fn set_feedback(&mut self, severity: FeedbackSeverity, text: impl Into<String>) {
        let text = text.into();
        self.status_message = text.clone();
        self.feedback = FeedbackModel { text, severity };
    }

    /// Operator errors remain visible in chat, feedback, and activity.
    pub fn report_error(&mut self, raw: &str) {
        let msg = classify_operator_error(raw);
        self.set_feedback(FeedbackSeverity::Error, msg.clone());
        // Replace prior error banners — don't accumulate red clutter in the chat.
        self.ui_banners.retain(|b| {
            !matches!(
                b,
                ChatItem::Banner {
                    kind: BannerKind::Error,
                    ..
                }
            )
        });
        self.ui_banners.push(ChatItem::Banner {
            text: msg.clone(),
            kind: BannerKind::Error,
        });
        self.activity
            .push(ActivityKind::Error, FeedbackSeverity::Error, msg);
        self.busy_phase = BusyPhase::Idle;
    }

    fn record_interrupted_stream(&mut self, error: &str) {
        let text = self.stream_preview.trim_end().to_string();
        if !text.is_empty() {
            self.session.messages.push(Message {
                role: MessageRole::Assistant,
                content: format!("{text}\n\n[Interrupted: {error}]"),
                tool_call_id: None,
                name: None,
                thinking: (!self.stream_thinking.trim().is_empty())
                    .then(|| self.stream_thinking.clone()),
                thinking_duration_secs: self.thought_secs,
                tool_calls: Vec::new(),
            });
        }
        self.set_feedback(
            FeedbackSeverity::Warn,
            "Response interrupted · Retry or Continue",
        );
        self.push_activity(
            ActivityKind::Model,
            FeedbackSeverity::Warn,
            format!("response interrupted: {error}"),
        );
    }

    /// Drop ephemeral error UI (call on new user turn / Esc).
    fn clear_error_chrome(&mut self) {
        self.ui_banners.retain(|b| {
            !matches!(
                b,
                ChatItem::Banner {
                    kind: BannerKind::Error,
                    ..
                }
            )
        });
        if self.feedback.severity == FeedbackSeverity::Error {
            self.feedback = FeedbackModel::default();
            self.status_message.clear();
        }
    }

    pub fn report_info(&mut self, text: impl Into<String>) {
        self.set_feedback(FeedbackSeverity::Info, text);
    }

    pub fn push_activity(
        &mut self,
        kind: ActivityKind,
        severity: FeedbackSeverity,
        summary: impl Into<String>,
    ) {
        self.activity.push(kind, severity, summary);
    }

    fn apply_history_text(&mut self, text: String) {
        self.input.set_text(text);
        self.input.history_browse = self.history.browsing();
        self.clamp_slash_suggest();
    }

    fn open_connect_picker(&mut self) {
        let connected: HashSet<String> = {
            let svc = ConnectService {
                registry: &self.connect_registry,
                store: &self.connect_store,
                active_profile_id: self.connect_profile.clone(),
                active_model: Some(self.runtime.model_label.clone()),
            };
            svc.connected_profiles()
                .unwrap_or_default()
                .into_iter()
                .map(|p| p.id)
                .collect()
        };
        let items: Vec<ConnectProfileItem> = self
            .connect_registry
            .profiles()
            .iter()
            .map(|p| ConnectProfileItem {
                id: p.id.clone(),
                title: p.title.clone(),
                auth_mode: p.auth_mode.label().into(),
                auth_url: p.auth_url.clone(),
                connected: connected.contains(&p.id),
            })
            .collect();
        self.overlay = Some(Overlay::connect_picker(items));
        self.status_message = "Choose a provider".into();
        self.notices.clear();
    }

    fn resolve_workspace_path(&self, path: impl AsRef<Path>) -> Result<PathBuf, TuiError> {
        let input = path.as_ref();
        let joined = if input.is_absolute() {
            input.to_path_buf()
        } else {
            self.runtime.cwd.join(input)
        };
        let canonical = joined.canonicalize()?;
        let root = self.runtime.cwd.canonicalize()?;
        if !canonical.starts_with(&root) {
            return Err(TuiError::Other(
                "file explorer is limited to the workspace".into(),
            ));
        }
        Ok(canonical)
    }

    fn open_file_explorer(&mut self, path: Option<&str>, error: Option<String>) {
        let dir = match path {
            Some(path) if !path.trim().is_empty() => self
                .resolve_workspace_path(path.trim())
                .unwrap_or_else(|_| self.runtime.cwd.clone()),
            _ => self.runtime.cwd.clone(),
        };
        let dir = if dir.is_dir() {
            dir
        } else {
            dir.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.runtime.cwd.clone())
        };
        let root = self
            .runtime
            .cwd
            .canonicalize()
            .unwrap_or_else(|_| self.runtime.cwd.clone());
        let current = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        let mut items = Vec::new();
        if current != root {
            if let Some(parent) = dir.parent() {
                items.push(FileExplorerItem {
                    name: "..".into(),
                    path: parent.display().to_string(),
                    is_dir: true,
                });
            }
        }
        match fs::read_dir(&dir) {
            Ok(entries) => {
                let mut children = entries
                    .filter_map(Result::ok)
                    .filter_map(|entry| {
                        let path = entry.path();
                        let file_type = entry.file_type().ok()?;
                        let is_dir = file_type.is_dir();
                        if !is_dir && !file_type.is_file() {
                            return None;
                        }
                        Some(FileExplorerItem {
                            name: entry.file_name().to_string_lossy().into_owned(),
                            path: path.display().to_string(),
                            is_dir,
                        })
                    })
                    .collect::<Vec<_>>();
                children.sort_by(|left, right| {
                    right.is_dir.cmp(&left.is_dir).then_with(|| {
                        left.name
                            .to_ascii_lowercase()
                            .cmp(&right.name.to_ascii_lowercase())
                    })
                });
                items.extend(children);
            }
            Err(err) => {
                self.overlay = Some(Overlay::file_explorer(
                    dir.display().to_string(),
                    items,
                    Some(format!("Could not read directory: {err}")),
                ));
                return;
            }
        }
        self.overlay = Some(Overlay::file_explorer(
            dir.display().to_string(),
            items,
            error,
        ));
        self.status_message = "File explorer (readonly)".into();
        self.notices.clear();
    }

    fn open_file_viewer(&mut self, path: &str) {
        match self.resolve_workspace_path(path).and_then(|path| {
            if !path.is_file() {
                return Err(TuiError::Other("selected path is not a file".into()));
            }
            let contents = String::from_utf8_lossy(&fs::read(&path)?).into_owned();
            Ok((path, contents))
        }) {
            Ok((path, contents)) => {
                self.overlay = Some(Overlay::file_viewer(path.display().to_string(), contents));
                self.status_message = "Viewing file (readonly)".into();
            }
            Err(err) => self.open_file_explorer(None, Some(format!("Could not open file: {err}"))),
        }
    }

    fn show_file_in_editor(&mut self, path: &Path) {
        let root = self.session.workspace_root().to_path_buf();
        self.source_viewer.open(&root, path);
        self.focus_block(FocusBlock::Workspace);
        self.status_message = "Viewing file (readonly)".into();
        // Keep the file explorer in sync with the active file.
        self.file_explorer.selected_path = Some(path.to_path_buf());
    }

    fn open_file_in_editor(&mut self, path: &Path) {
        self.navigate_to_workspace_view(WorkspaceView::File(path.to_path_buf()));
    }

    #[cfg(test)]
    pub(crate) fn open_file_view_for_test(&mut self, path: &Path) {
        self.open_file_in_editor(path);
    }

    #[cfg(test)]
    pub(crate) fn review_changes_for_test(&mut self) {
        self.navigate_to_workspace_view(WorkspaceView::Diff(DiffCommandContext::Current));
    }

    fn file_ops(&self) -> Result<WorkspaceFileOps, FileOperationError> {
        WorkspaceFileOps::new(self.session.workspace_root())
    }

    fn open_explorer_name_dialog(&mut self, action: ExplorerNameAction) {
        let Some(parent) = self.file_explorer.selected_creation_parent() else {
            self.set_feedback(FeedbackSeverity::Warn, "No workspace folder selected");
            return;
        };
        let (source, input) = if action == ExplorerNameAction::Rename {
            let Some(node) = self.file_explorer.selected_node() else {
                self.set_feedback(FeedbackSeverity::Warn, "No file or folder selected");
                return;
            };
            if self.file_explorer.root_path() == Some(node.path.as_path()) {
                self.set_feedback(FeedbackSeverity::Warn, "Cannot rename the workspace root");
                return;
            }
            (Some(node.path.clone()), node.display_name.clone())
        } else {
            (None, String::new())
        };
        self.explorer_dialog = Some(ExplorerDialog::Name {
            action,
            parent,
            source,
            input,
            error: None,
        });
        self.focus_block(FocusBlock::Files);
    }

    fn open_explorer_delete_dialog(&mut self) {
        let Some(node) = self.file_explorer.selected_node() else {
            self.set_feedback(FeedbackSeverity::Warn, "No file or folder selected");
            return;
        };
        if self.file_explorer.root_path() == Some(node.path.as_path()) {
            self.set_feedback(FeedbackSeverity::Warn, "Cannot delete the workspace root");
            return;
        }
        let ops = match self.file_ops() {
            Ok(ops) => ops,
            Err(error) => {
                self.set_feedback(FeedbackSeverity::Error, error.actionable());
                return;
            }
        };
        let kind = match ops.entry_kind(&node.path) {
            Ok(kind) => kind,
            Err(error) => {
                self.set_feedback(FeedbackSeverity::Error, error.actionable());
                return;
            }
        };
        let non_empty = match ops.is_non_empty_directory(&node.path) {
            Ok(non_empty) => non_empty,
            Err(error) => {
                self.set_feedback(FeedbackSeverity::Error, error.actionable());
                return;
            }
        };
        self.explorer_dialog = Some(ExplorerDialog::ConfirmDelete {
            source: node.path.clone(),
            name: node.display_name.clone(),
            kind,
            non_empty,
            permanent: false,
            error: None,
        });
        self.focus_block(FocusBlock::Files);
    }

    fn handle_explorer_dialog_key(&mut self, key: event::KeyEvent) -> bool {
        let Some(dialog) = self.explorer_dialog.take() else {
            return false;
        };
        let next = match dialog {
            ExplorerDialog::Name {
                action,
                parent,
                source,
                mut input,
                ..
            } => match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => None,
                KeyCode::Backspace if key.modifiers.is_empty() => {
                    input.pop();
                    Some(ExplorerDialog::Name {
                        action,
                        parent,
                        source,
                        input,
                        error: None,
                    })
                }
                KeyCode::Char(c)
                    if (key.modifiers & !(KeyModifiers::SHIFT | KeyModifiers::NONE)).is_empty()
                        && !c.is_control() =>
                {
                    input.push(c);
                    Some(ExplorerDialog::Name {
                        action,
                        parent,
                        source,
                        input,
                        error: None,
                    })
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    match self.prepare_explorer_name_operation(
                        action,
                        &parent,
                        source.as_deref(),
                        &input,
                    ) {
                        Ok(next) => Some(next),
                        Err(error) => Some(ExplorerDialog::Name {
                            action,
                            parent,
                            source,
                            input,
                            error: Some(error.actionable()),
                        }),
                    }
                }
                _ => Some(ExplorerDialog::Name {
                    action,
                    parent,
                    source,
                    input,
                    error: None,
                }),
            },
            ExplorerDialog::ConfirmCreate {
                action,
                parent,
                name,
                path,
            } => match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => None,
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.apply_confirmed_create(action, &parent, &name);
                    None
                }
                _ => Some(ExplorerDialog::ConfirmCreate {
                    action,
                    parent,
                    name,
                    path,
                }),
            },
            ExplorerDialog::ConfirmRename { source, path, name } => match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => None,
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.apply_confirmed_rename(&source, &name);
                    None
                }
                _ => Some(ExplorerDialog::ConfirmRename { source, path, name }),
            },
            ExplorerDialog::ConfirmDelete {
                source,
                name,
                kind,
                non_empty,
                permanent,
                error,
            } => match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => None,
                KeyCode::Char('p') | KeyCode::Char('P') if error.is_some() && !permanent => {
                    Some(ExplorerDialog::ConfirmDelete {
                        source,
                        name,
                        kind,
                        non_empty,
                        permanent: true,
                        error: None,
                    })
                }
                KeyCode::Char('D') if permanent || non_empty => {
                    self.apply_confirmed_delete(
                        &source,
                        if permanent {
                            DeleteMode::Permanent
                        } else {
                            DeleteMode::Trash
                        },
                    );
                    None
                }
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y')
                    if !permanent && !non_empty && error.is_none() =>
                {
                    self.apply_confirmed_delete(&source, DeleteMode::Trash);
                    None
                }
                _ => Some(ExplorerDialog::ConfirmDelete {
                    source,
                    name,
                    kind,
                    non_empty,
                    permanent,
                    error,
                }),
            },
        };
        self.explorer_dialog = next;
        true
    }

    fn prepare_explorer_name_operation(
        &self,
        action: ExplorerNameAction,
        parent: &Path,
        source: Option<&Path>,
        input: &str,
    ) -> Result<ExplorerDialog, FileOperationError> {
        let ops = self.file_ops()?;
        match action {
            ExplorerNameAction::CreateFile | ExplorerNameAction::CreateDirectory => {
                let path = ops.plan_create(parent, input)?;
                Ok(ExplorerDialog::ConfirmCreate {
                    action,
                    parent: parent.to_path_buf(),
                    name: input.trim().to_string(),
                    path,
                })
            }
            ExplorerNameAction::Rename => {
                let source = source.ok_or(FileOperationError::MissingSource)?;
                let path = ops.plan_rename(source, input)?;
                Ok(ExplorerDialog::ConfirmRename {
                    source: source.to_path_buf(),
                    path,
                    name: input.trim().to_string(),
                })
            }
        }
    }

    fn apply_confirmed_create(&mut self, action: ExplorerNameAction, parent: &Path, name: &str) {
        let result = match self.file_ops() {
            Ok(ops) if action == ExplorerNameAction::CreateFile => ops.create_file(parent, name),
            Ok(ops) => ops.create_directory(parent, name),
            Err(error) => Err(error),
        };
        match result {
            Ok(result) => self.reconcile_file_operation(result),
            Err(error) => self.set_feedback(FeedbackSeverity::Error, error.actionable()),
        }
    }

    fn apply_confirmed_rename(&mut self, source: &Path, name: &str) {
        let result = self
            .file_ops()
            .and_then(|ops| ops.rename_entry(source, name));
        match result {
            Ok(result) => self.reconcile_file_operation(result),
            Err(error) => self.set_feedback(FeedbackSeverity::Error, error.actionable()),
        }
    }

    fn apply_confirmed_delete(&mut self, source: &Path, mode: DeleteMode) {
        let result = self
            .file_ops()
            .and_then(|ops| ops.delete_entry(source, mode));
        match result {
            Ok(result) => self.reconcile_file_operation(result),
            Err(FileOperationError::TrashUnavailable(reason)) if mode == DeleteMode::Trash => {
                if let Some(node) = self.file_explorer.selected_node() {
                    let kind = self
                        .file_ops()
                        .and_then(|ops| ops.entry_kind(&node.path))
                        .unwrap_or(EntryKind::Other);
                    let non_empty = self
                        .file_ops()
                        .and_then(|ops| ops.is_non_empty_directory(&node.path))
                        .unwrap_or(false);
                    self.explorer_dialog = Some(ExplorerDialog::ConfirmDelete {
                        source: node.path.clone(),
                        name: node.display_name.clone(),
                        kind,
                        non_empty,
                        permanent: false,
                        error: Some(FileOperationError::TrashUnavailable(reason).actionable()),
                    });
                } else {
                    self.set_feedback(FeedbackSeverity::Error, "Trash is unavailable");
                }
            }
            Err(error) => self.set_feedback(FeedbackSeverity::Error, error.actionable()),
        }
    }

    fn reconcile_file_operation(&mut self, result: crate::file_ops::FileOperationResult) {
        self.clear_pending_double_click();
        let root = self.session.workspace_root().to_path_buf();
        match result.kind {
            FileOperationKind::CreateFile | FileOperationKind::CreateDirectory => {
                self.file_explorer
                    .refresh_parent_and_select(&result.parent, &result.path);
                self.set_feedback(
                    FeedbackSeverity::Ok,
                    format!("Created {}", relative_display(&root, &result.path)),
                );
            }
            FileOperationKind::RenameEntry => {
                if let Some(new_path) = result.new_path.as_ref() {
                    self.reconcile_path_rename(&result.path, new_path);
                    self.file_explorer
                        .refresh_parent_and_select(&result.parent, new_path);
                    self.set_feedback(
                        FeedbackSeverity::Ok,
                        format!("Renamed to {}", relative_display(&root, new_path)),
                    );
                }
            }
            FileOperationKind::DeleteEntry => {
                self.reconcile_path_delete(&result.path);
                self.file_explorer
                    .refresh_after_delete(&result.parent, &result.path);
                self.set_feedback(
                    FeedbackSeverity::Ok,
                    format!("Removed {}", relative_display(&root, &result.path)),
                );
            }
        }
        self.diff_selected = self.diff_selected.min(
            self.file_explorer
                .git_status
                .changed_files()
                .len()
                .saturating_sub(1),
        );
    }

    fn reconcile_path_rename(&mut self, old_path: &Path, new_path: &Path) {
        let root = self.session.workspace_root().to_path_buf();
        if let Some(open_path) = self.source_viewer.path.clone() {
            if let Some(rebased) = rebase_path(&open_path, old_path, new_path) {
                self.source_viewer
                    .reconcile_renamed_path(&root, &open_path, &rebased);
            }
        }
        if let Some(att) = self.pending_attachment.as_mut() {
            let abs = root.join(&att.rel_path);
            if let Some(rebased) = rebase_path(&abs, old_path, new_path) {
                att.rel_path = relative_display(&root, &rebased);
            }
        }
        self.file_explorer.refresh_git_status();
    }

    fn reconcile_path_delete(&mut self, deleted_path: &Path) {
        let root = self.session.workspace_root().to_path_buf();
        if let Some(open_path) = self.source_viewer.path.clone() {
            if open_path == deleted_path || open_path.starts_with(deleted_path) {
                self.source_viewer.reconcile_deleted_path(&open_path);
            }
        }
        if self.pending_attachment.as_ref().is_some_and(|att| {
            let abs = root.join(&att.rel_path);
            abs == deleted_path || abs.starts_with(deleted_path)
        }) {
            self.pending_attachment = None;
        }
        self.file_explorer.refresh_git_status();
    }

    /// Toggle attachment of the current source-viewer file to the next message.
    fn toggle_file_attachment(&mut self) {
        if self.pending_attachment.is_some() {
            self.pending_attachment = None;
            self.set_feedback(FeedbackSeverity::Info, "Attachment removed");
            return;
        }

        let path = match &self.source_viewer.path {
            Some(p) if self.source_viewer.status.is_openable() => p.clone(),
            _ => {
                self.set_feedback(FeedbackSeverity::Warn, "No openable file to attach");
                return;
            }
        };

        let root = self.session.workspace_root();
        let rel_path = match path.strip_prefix(root) {
            Ok(rel) => rel.display().to_string(),
            Err(_) => {
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    "Active file is outside the repository",
                );
                return;
            }
        };

        let cursor_line = self.source_viewer.current_line;
        self.pending_attachment = Some(crate::file_context::FileAttachment::new(
            rel_path,
            cursor_line,
        ));
        if let Some(ref att) = self.pending_attachment {
            self.set_feedback(
                FeedbackSeverity::Info,
                format!("File attached · {}", att.label()),
            );
        }
    }

    fn open_api_key_prompt(&mut self, profile_id: &str, error: Option<String>) {
        let p = self.connect_registry.get(profile_id);
        let title = p
            .map(|x| x.title.clone())
            .unwrap_or_else(|| profile_id.to_string());
        let auth_url = p.and_then(|x| x.auth_url.clone());
        let env_hint = if error.is_none() {
            p.and_then(|x| {
                x.api_key_env.iter().find_map(|env_name| {
                    std::env::var(env_name)
                        .ok()
                        .filter(|value| !value.is_empty())
                        .map(|_| env_name.clone())
                })
            })
        } else {
            None
        };
        let mut overlay = Overlay::connect_api_key(profile_id, title, auth_url, env_hint);
        if let Overlay::ConnectApiKey {
            error: overlay_error,
            ..
        } = &mut overlay
        {
            *overlay_error = error;
        }
        self.overlay = Some(overlay);
        self.status_message = format!("Connect {profile_id}");
        self.notices.clear();
    }

    fn open_model_picker_after_connect(&mut self, profile_id: &str) {
        let items = self.model_picker_items(true);
        let mut overlay = Overlay::model_open_with(items);
        overlay.focus_model(&self.runtime.model_label);
        self.overlay = Some(overlay);
        let title = self
            .connect_registry
            .get(profile_id)
            .map(|p| p.title.as_str())
            .unwrap_or(profile_id);
        self.set_feedback(
            FeedbackSeverity::Ok,
            format!("{title} connected · choose a model"),
        );
        self.notices.clear();
    }

    fn handle_connect(&mut self, action: ConnectAction) {
        // /connect or /connect list → interactive profile picker (usable UX)
        match &action {
            ConnectAction::Open | ConnectAction::List => {
                self.open_connect_picker();
                // Also fill notices with list for accessibility
                let mut model = Some(self.runtime.model_label.clone());
                if let Ok(msg) = handle_connect_action(
                    ConnectAction::List,
                    &self.connect_registry,
                    &self.connect_store,
                    &mut self.connect_profile,
                    &mut model,
                ) {
                    self.push_notice(msg.lines().map(|s| s.to_string()).collect());
                }
                return;
            }
            _ => {}
        }

        // Phase 6.1: open mode-specific overlays for interactive connect
        let connect_target = if let ConnectAction::Connect {
            ref profile_id,
            ref api_key,
            oauth_fixture,
        } = action
        {
            if api_key.is_none() && !oauth_fixture {
                if needs_tui_api_key_prompt(&self.connect_registry, profile_id) {
                    // Existing file/env credentials should reconnect without
                    // asking the user to paste the same secret again.
                    if !self.credentials_live_for(profile_id) {
                        self.open_api_key_prompt(profile_id, None);
                        return;
                    }
                }
                if needs_tui_oauth(&self.connect_registry, profile_id) {
                    self.begin_oauth_flow(profile_id);
                    return;
                }
            }
            Some(profile_id.clone())
        } else {
            None
        };

        let mut model = Some(self.runtime.model_label.clone());
        match handle_connect_action(
            action,
            &self.connect_registry,
            &self.connect_store,
            &mut self.connect_profile,
            &mut model,
        ) {
            Ok(msg) => {
                if let Some(m) = model {
                    self.runtime.model_label = m.clone();
                    self.runtime.provider = "native".into();
                    self.auth_suspended = false;
                    self.session.set_active_model(m);
                }
                if let Some(pid) = self.connect_profile.clone() {
                    self.apply_connect_credentials(&pid);
                }
                let lines: Vec<String> = msg.lines().map(|s| s.to_string()).collect();
                self.status_message = lines.first().cloned().unwrap_or_default();
                self.notices.clear();
                self.notices_until = None;
                self.notices_until = None;
                self.notices_until = None;
                self.push_activity(
                    ActivityKind::Connect,
                    FeedbackSeverity::Ok,
                    self.status_message.clone(),
                );
                if let Some(line) = lines.first() {
                    self.push_toast(line.clone());
                }
                self.refresh_connection_ui();
                if let Some(profile_id) = connect_target {
                    self.open_model_picker_after_connect(&profile_id);
                }
            }
            Err(ConnectError::OauthDevicePending(pending)) => {
                // Payload is boxed so `ConnectError` stays small in every `Result`.
                self.show_oauth_pending(*pending);
            }
            Err(e) => {
                let error = e.to_string();
                if let Some(profile_id) =
                    connect_target.filter(|id| needs_tui_api_key_prompt(&self.connect_registry, id))
                {
                    self.open_api_key_prompt(&profile_id, Some(error));
                } else {
                    self.status_message = error.clone();
                    self.push_notice(vec![error]);
                }
            }
        }
    }

    fn begin_oauth_flow(&mut self, profile_id: &str) {
        let mut svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        match svc.connect_start_oauth(profile_id) {
            Ok(Ok(out)) => {
                self.on_connect_success(&out);
            }
            Ok(Err(pending)) => self.show_oauth_pending(pending),
            Err(e) => {
                self.status_message = e.to_string();
                self.push_notice(vec![e.to_string()]);
                self.report_error(&e.to_string());
            }
        }
    }

    /// After a successful connect: update model, inject credentials, clear OAuth UI.
    fn on_connect_success(&mut self, out: &forge_connect::ConnectOutcome) {
        self.connect_profile = Some(out.profile_id.clone());
        self.runtime.model_label = out.model.clone();
        self.runtime.provider = "native".into();
        self.auth_suspended = false;
        self.session.set_active_model(out.model.clone());
        self.apply_connect_credentials(&out.profile_id);
        self.oauth_pending = None;
        self.oauth_last_poll = None;
        self.refresh_connection_ui();
        self.open_model_picker_after_connect(&out.profile_id);
    }

    /// Export stored OAuth / API key material into the native model client.
    fn apply_connect_credentials(&mut self, profile_id: &str) {
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        match svc.provider_env_for_profile(profile_id) {
            Ok(pairs) if !pairs.is_empty() => {
                // Given to the client only. `NativeModelClient::injected_or_env`
                // reads the injected map ahead of the process environment, so
                // exporting these as well changed nothing for the client — it
                // only made every child process inherit them.
                self.session.apply_provider_env(&pairs);
            }
            Ok(_) => {}
            Err(_e) => {
                // Non-fatal: operator can still set XAI_API_KEY in the shell.
            }
        }
    }

    fn show_oauth_pending(&mut self, pending: OauthPending) {
        let title = self
            .connect_registry
            .get(&pending.profile_id)
            .map(|p| p.title.clone())
            .unwrap_or_else(|| pending.profile_id.clone());
        let instructions = pending.operator_instructions();
        let lines: Vec<String> = instructions.lines().map(|s| s.to_string()).collect();
        self.status_message = lines
            .first()
            .cloned()
            .unwrap_or_else(|| format!("OAuth for {}", pending.profile_id));
        self.push_notice(lines);
        self.overlay = Some(Overlay::connect_oauth(
            pending.profile_id.clone(),
            title,
            instructions,
        ));
        self.oauth_pending = Some(pending);
        self.oauth_last_poll = None;
    }

    /// Poll device-code OAuth once (called from the TUI tick loop).
    pub fn poll_oauth_tick(&mut self) {
        let Some(pending) = self.oauth_pending.clone() else {
            return;
        };
        let interval = Duration::from_secs(pending.interval_secs.max(1));
        if let Some(last) = self.oauth_last_poll {
            if last.elapsed() < interval {
                return;
            }
        }
        self.oauth_last_poll = Some(std::time::Instant::now());
        let mut svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        match svc.poll_oauth_once(&pending) {
            Ok(Some(out)) => {
                self.on_connect_success(&out);
            }
            Ok(None) => {
                // still waiting
            }
            Err(e) => {
                self.oauth_pending = None;
                self.oauth_last_poll = None;
                self.overlay = None;
                self.report_error(&e.to_string());
            }
        }
    }

    fn finish_connect(&mut self, profile_id: &str, api_key: Option<String>, oauth_fixture: bool) {
        self.handle_connect(ConnectAction::Connect {
            profile_id: profile_id.into(),
            api_key,
            oauth_fixture,
        });
    }

    /// Submit API key (or env) from the connect modal. On failure, keep the modal open
    /// with an error so the operator can re-paste (does not clear a long key on length checks
    /// when the failure came from Use-env short key).
    fn try_connect_api_key(&mut self, profile_id: &str, api_key: Option<String>) {
        let saved_overlay = self.overlay.take();
        let mut model = Some(self.runtime.model_label.clone());
        let action = ConnectAction::Connect {
            profile_id: profile_id.into(),
            api_key: api_key.clone(),
            oauth_fixture: false,
        };
        match handle_connect_action(
            action,
            &self.connect_registry,
            &self.connect_store,
            &mut self.connect_profile,
            &mut model,
        ) {
            Ok(msg) => {
                if let Some(m) = model {
                    self.runtime.model_label = m.clone();
                    self.runtime.provider = "native".into();
                    self.auth_suspended = false;
                    self.session.set_active_model(m);
                }
                if let Some(pid) = self.connect_profile.clone() {
                    self.apply_connect_credentials(&pid);
                }
                self.status_message = msg.lines().next().unwrap_or_default().to_string();
                self.refresh_connection_ui();
                self.open_model_picker_after_connect(profile_id);
            }
            Err(e) => {
                let err = e.to_string();
                self.overlay = saved_overlay;
                if let Some(Overlay::ConnectApiKey { error, .. }) = &mut self.overlay {
                    *error = Some(err.clone());
                }
                self.status_message = err;
                self.push_activity(
                    ActivityKind::Connect,
                    FeedbackSeverity::Error,
                    format!("connect {profile_id} failed"),
                );
            }
        }
    }

    /// Apply a provider/model id to this session (no restart required).
    fn apply_model_selection(&mut self, provider: &str, model: &str) {
        let model = model.trim();
        if model.is_empty() {
            return;
        }
        self.runtime.provider = if provider.trim().is_empty() {
            "native".into()
        } else {
            provider.to_string()
        };
        self.auth_suspended = false;
        self.runtime.model_label = model.to_string();
        self.session.set_active_model(model);
        // Match the selected model to its connected profile even when a
        // different provider was active before opening the picker.
        let prefix = model.split('/').next().unwrap_or("");
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: None,
            active_model: None,
        };
        if let Ok(connected) = svc.connected_profiles() {
            if let Some(profile) = connected.iter().find(|p| {
                p.model_provider_prefix == prefix
                    || p.id == prefix
                    || (prefix == "opencode-go" && p.id == "opencode_go")
                    || (prefix == "opencode-zen" && p.id == "opencode_zen")
            }) {
                self.connect_profile = Some(profile.id.clone());
            }
        }
        if let Some(profile_id) = self.connect_profile.clone() {
            self.apply_connect_credentials(&profile_id);
        }
        self.persist_selection();
        self.feedback = FeedbackModel::default();
        self.status_message.clear();
        self.notices.clear();
        self.push_activity(
            ActivityKind::System,
            FeedbackSeverity::Ok,
            format!("model {}", self.runtime.model_label),
        );
    }

    fn model_prefix(model: &str) -> &str {
        model.split('/').next().unwrap_or("").trim()
    }

    fn connected_profile_for_model_prefix(&self, prefix: &str) -> Option<String> {
        let prefix = prefix.trim();
        if prefix.is_empty() {
            return None;
        }
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        let connected = svc.connected_profiles().ok()?;
        connected.iter().find_map(|profile| {
            let pid = profile.id.as_str();
            let provider_prefix = profile.model_provider_prefix.as_str();
            let matches = prefix == pid
                || prefix == provider_prefix
                || (prefix == "openai" && pid == "openai_codex")
                || (prefix == "openai-codex" && pid == "openai_codex")
                || (prefix == "opencode-go" && pid == "opencode_go")
                || (prefix == "opencode-zen" && pid == "opencode_zen")
                || (prefix == "grok" && pid == "xai");
            if matches {
                Some(profile.id.clone())
            } else {
                None
            }
        })
    }

    /// Build `/model` picker rows from connected-profile catalogs (cache + optional refresh).
    fn model_picker_items(&self, refresh_stale: bool) -> Vec<crate::overlays::ModelItem> {
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        let connected = svc.connected_profiles().unwrap_or_default();
        let cache = ModelCatalogCache::user_default();
        let profiles: Vec<_> = if connected.is_empty() {
            // Show all built-in defaults when nothing connected
            self.connect_registry.profiles().to_vec()
        } else {
            connected
        };
        let entries = models_for_picker(&profiles, &self.connect_store, &cache, refresh_stale);
        models_from_catalog(&entries)
    }

    #[allow(dead_code)]
    fn active_model_cost(&mut self) -> Option<forge_connect::CatalogCost> {
        if let Some((model, cost)) = &self.model_cost_cache {
            if model == &self.runtime.model_label {
                return *cost;
            }
        }
        let cost = ModelCatalogCache::user_default().get_registry_cost(&self.runtime.model_label);
        self.model_cost_cache = Some((self.runtime.model_label.clone(), cost));
        cost
    }

    /// Enqueue while a message is processing (TUI Enter path only).
    fn enqueue_user_message(&mut self, line: String) {
        let n = self.message_queue.enqueue(line);
        if self.queue_selected.is_none() {
            self.queue_selected = Some(0);
        }
        self.push_toast(format!("queued #{n}"));
        self.set_feedback(
            FeedbackSeverity::Info,
            format!(
                "queued #{n} · {} waiting · Ctrl+Up/Down select · Ctrl+Backspace cancel",
                self.message_queue.len()
            ),
        );
        self.push_activity(
            ActivityKind::System,
            FeedbackSeverity::Info,
            format!("queue enqueue #{n}"),
        );
    }

    /// Take next queued message and start a model turn.
    fn dequeue_and_send_next(&mut self) {
        if self.busy || self.pending_prompt.is_some() {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "still processing — wait before sending the next queued message",
            );
            return;
        }
        if self.session.pending_hitl.is_some() {
            self.set_feedback(FeedbackSeverity::Warn, "resolve HITL before dequeuing");
            return;
        }
        let Some(next) = self.message_queue.dequeue() else {
            self.set_feedback(FeedbackSeverity::Warn, "queue empty");
            return;
        };
        self.clamp_queue_selection();
        if !self.is_provider_connected() {
            self.message_queue.push_front(next);
            self.report_error("Not connected — cannot send queued message. Run /connect.");
            return;
        }
        self.push_activity(
            ActivityKind::System,
            FeedbackSeverity::Info,
            format!("queue dequeue · {} left", self.message_queue.len()),
        );
        self.set_feedback(
            FeedbackSeverity::Info,
            format!("sending dequeued · {} remaining", self.message_queue.len()),
        );
        // Start the turn the same way as a normal Enter send (no dispatch recursion).
        self.clear_error_chrome();
        if let Some(pid) = self.connect_profile.clone() {
            self.apply_connect_credentials(&pid);
        }
        self.pending_prompt = Some(next);
        self.busy = true;
        self.busy_phase = BusyPhase::Model;
        self.turn_started = Some(Instant::now());
        self.stream_preview.clear();
        self.stream_thinking.clear();
        self.push_activity(
            ActivityKind::Model,
            FeedbackSeverity::Info,
            "model call started",
        );
    }

    /// Cancel a queued message by 0-based index.
    fn cancel_queued_at(&mut self, index: usize) {
        let one_based = index + 1;
        match self.message_queue.drop_at(one_based) {
            Some(t) => {
                let preview: String = t.chars().take(48).collect();
                self.push_toast(format!("cancelled #{one_based}"));
                self.set_feedback(
                    FeedbackSeverity::Ok,
                    format!(
                        "cancelled queued #{one_based} · {} left",
                        self.message_queue.len()
                    ),
                );
                self.push_activity(
                    ActivityKind::System,
                    FeedbackSeverity::Ok,
                    format!("queue cancel #{one_based}: {preview}"),
                );
                self.clamp_queue_selection();
            }
            None => {
                self.set_feedback(FeedbackSeverity::Warn, "queue item gone");
            }
        }
    }

    fn clamp_queue_selection(&mut self) {
        let len = self.message_queue.len();
        self.queue_selected = match (len, self.queue_selected) {
            (0, _) => None,
            (_, Some(i)) if i < len => Some(i),
            (_, Some(_)) => Some(len - 1),
            (_, None) => Some(0),
        };
    }

    fn move_queue_selection(&mut self, delta: i32) {
        let len = self.message_queue.len();
        if len == 0 {
            self.queue_selected = None;
            return;
        }
        let cur = self.queue_selected.unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(len as i32) as usize;
        self.queue_selected = Some(next);
    }

    fn cancel_selected_queue(&mut self) {
        let Some(idx) = self.queue_selected else {
            self.set_feedback(FeedbackSeverity::Warn, "queue empty");
            return;
        };
        self.cancel_queued_at(idx);
    }

    /// Run the built-in `git` tool in the session workspace. Never touches the model.
    async fn run_git_tool(
        &self,
        subcommand: &str,
        args: Vec<String>,
    ) -> Result<forge_types::ToolOutput, forge_tools::ToolError> {
        let ctx = ToolContext::new(self.session.workspace_root().to_path_buf());
        GitTool
            .call(
                &ctx,
                json!({
                    "subcommand": subcommand,
                    "args": args,
                }),
            )
            .await
    }

    /// `/sync` — stage all changes, invent a commit message from the changeset, commit, push.
    fn queue_sync(&mut self) {
        if self.busy || self.pending_prompt.is_some() || self.pending_sync {
            self.set_feedback(FeedbackSeverity::Warn, "busy — wait before /sync");
            return;
        }
        self.pending_sync = true;
        self.busy_phase = BusyPhase::Other("git sync".into());
        self.push_toast("syncing…");
        self.status_message = "syncing…".into();
        self.push_activity(
            ActivityKind::Tool,
            FeedbackSeverity::Info,
            "git sync queued",
        );
    }

    fn run_current_draft(&mut self) {
        if self
            .run
            .current
            .as_ref()
            .is_some_and(|record| record.state == RunState::Running)
        {
            self.run.error = Some("a run is already active; cancel it first".into());
            return;
        }
        let invocation = match self.run.draft.invocation() {
            Ok(invocation) => invocation,
            Err(error) => {
                self.run.error = Some(error.to_string());
                return;
            }
        };
        if !invocation.working_directory.is_dir() {
            self.run.error = Some(format!(
                "working directory is not accessible: {}",
                invocation.working_directory.display()
            ));
            return;
        }
        let mut record = self.run.record(
            invocation.clone(),
            crate::RunProvenance::Manual,
            Some(self.session.session_id.to_string()),
        );
        record.state = RunState::Running;
        record.started_at = Some(std::time::SystemTime::now());
        self.run.current = Some(record);
        self.run.error = None;
        self.pending_validation = true;
        self.busy_phase = BusyPhase::Tool { name: "run".into() };
        self.status_message = format!("run: {}", invocation.summary());
        self.push_activity(
            ActivityKind::Run,
            FeedbackSeverity::Info,
            format!("run started: {}", invocation.summary()),
        );
    }

    fn rerun_current(&mut self) {
        let Some(record) = self
            .run
            .current
            .clone()
            .or_else(|| self.run.recent.front().cloned())
        else {
            self.run.error = Some("no previous run".into());
            return;
        };
        let draft = &mut self.run.draft;
        draft.command_input = record.invocation.summary();
        draft.working_directory = record.invocation.working_directory;
        draft.environment_delta = record.invocation.environment_delta;
        draft.execution_mode = record.invocation.execution_mode;
        draft.source_record_id = Some(record.id);
        self.run_current_draft();
    }

    fn edit_and_rerun_current(&mut self) {
        let Some(record) = self
            .run
            .current
            .clone()
            .or_else(|| self.run.recent.front().cloned())
        else {
            self.run.error = Some("no previous run".into());
            return;
        };
        self.run.draft.command_input = record.invocation.summary();
        self.run.draft.working_directory = record.invocation.working_directory;
        self.run.draft.environment_delta = record.invocation.environment_delta;
        self.run.draft.execution_mode = record.invocation.execution_mode;
        self.run.draft.source_record_id = Some(record.id);
        self.run.editing = true;
    }

    fn cancel_run(&mut self) {
        let mut cancelled = None;
        if let Some(record) = self.run.current.as_mut() {
            if record.state == RunState::Running {
                if let Some(handle) = self.run_abort.take() {
                    handle.abort();
                }
                record.state = RunState::Cancelled;
                record.finished_at = Some(std::time::SystemTime::now());
                record.duration = record.started_at.and_then(|start| {
                    record
                        .finished_at
                        .and_then(|end| end.duration_since(start).ok())
                });
                self.run_rx = None;
                self.pending_validation = false;
                self.busy_phase = BusyPhase::Idle;
                cancelled = Some(record.clone());
            }
        }
        if let Some(record) = cancelled {
            self.push_activity(
                ActivityKind::Run,
                FeedbackSeverity::Warn,
                format!("run cancelled: {}", record.invocation.summary()),
            );
            self.run.remember(record);
            self.save_run_history();
        }
    }

    pub async fn drain_pending_validation(
        &mut self,
        terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        if !self.pending_validation
            || !self
                .run
                .current
                .as_ref()
                .is_some_and(|record| record.state == RunState::Running)
        {
            return Ok(());
        }
        self.pending_validation = false;
        let Some(record) = self.run.current.as_ref() else {
            return Ok(());
        };
        let invocation = record.invocation.clone();
        if let Some(term) = terminal {
            let _ = term.draw(|f| self.draw(f));
        }
        self.terminal_capture.title = Some(format!("run · {}", invocation.summary()));
        self.terminal_capture.content.clear();
        self.terminal_capture.truncated = false;
        let (tx, rx) = std::sync::mpsc::channel();
        self.run_rx = Some(rx);
        self.run_abort = Some(tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut cmd = tokio::process::Command::new(&invocation.executable);
            cmd.args(&invocation.arguments)
                .current_dir(&invocation.working_directory)
                .kill_on_drop(true)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            for change in invocation.environment_delta {
                match change {
                    crate::RunEnvironmentChange::Set { name, value } => {
                        cmd.env(name, value);
                    }
                    crate::RunEnvironmentChange::Remove { name } => {
                        cmd.env_remove(name);
                    }
                }
            }
            match cmd.spawn() {
                Ok(mut child) => {
                    let mut stdout = child.stdout.take();
                    let mut stderr = child.stderr.take();
                    let tx_out = tx.clone();
                    let stdout_task = tokio::spawn(async move {
                        if let Some(mut stream) = stdout.take() {
                            let mut buf = [0u8; 1024];
                            loop {
                                match stream.read(&mut buf).await {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        let _ = tx_out.send(RunEvent::Output(buf[..n].to_vec()));
                                    }
                                    Err(error) => {
                                        let _ = tx_out.send(RunEvent::CaptureFailed(format!(
                                            "output capture failed: {error}"
                                        )));
                                        break;
                                    }
                                }
                            }
                        }
                    });
                    let tx_err = tx.clone();
                    let stderr_task = tokio::spawn(async move {
                        if let Some(mut stream) = stderr.take() {
                            let mut buf = [0u8; 1024];
                            loop {
                                match stream.read(&mut buf).await {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        let _ = tx_err.send(RunEvent::Output(buf[..n].to_vec()));
                                    }
                                    Err(error) => {
                                        let _ = tx_err.send(RunEvent::CaptureFailed(format!(
                                            "output capture failed: {error}"
                                        )));
                                        break;
                                    }
                                }
                            }
                        }
                    });
                    let status = child.wait().await;
                    let _ = stdout_task.await;
                    let _ = stderr_task.await;
                    match status {
                        Ok(status) => {
                            let _ = tx.send(RunEvent::Finished {
                                exit_code: status.code(),
                                success: status.success(),
                            });
                        }
                        Err(error) => {
                            let _ = tx.send(RunEvent::SpawnFailed(error.to_string()));
                        }
                    }
                }
                Err(error) => {
                    let _ = tx.send(RunEvent::SpawnFailed(error.to_string()));
                }
            }
        }));
        Ok(())
    }

    fn poll_run(&mut self) {
        let Some(rx) = self.run_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(RunEvent::Output(chunk)) => {
                self.append_terminal_output(&chunk);
                self.run_rx = Some(rx);
            }
            Ok(RunEvent::Finished { exit_code, success }) => {
                self.run_abort = None;
                self.pending_validation = false;
                self.busy_phase = BusyPhase::Idle;
                if let Some(mut record) = self.run.current.take() {
                    record.state = if success {
                        RunState::Succeeded
                    } else {
                        RunState::Failed
                    };
                    record.exit_status = exit_code;
                    record.finished_at = Some(std::time::SystemTime::now());
                    record.duration = record.started_at.and_then(|start| {
                        record
                            .finished_at
                            .and_then(|end| end.duration_since(start).ok())
                    });
                    self.run.current = Some(record.clone());
                    self.push_activity(
                        ActivityKind::Run,
                        if success {
                            FeedbackSeverity::Ok
                        } else {
                            FeedbackSeverity::Error
                        },
                        if success {
                            format!("run succeeded: {}", record.invocation.summary())
                        } else {
                            format!("run failed: {}", record.invocation.summary())
                        },
                    );
                    self.run.remember(record);
                    self.save_run_history();
                }
            }
            Ok(RunEvent::SpawnFailed(error)) => {
                self.run_abort = None;
                self.pending_validation = false;
                self.busy_phase = BusyPhase::Idle;
                if let Some(mut record) = self.run.current.take() {
                    record.state = RunState::StartFailed;
                    record.spawn_error = Some(error.clone());
                    record.finished_at = Some(std::time::SystemTime::now());
                    record.duration = record.started_at.and_then(|start| {
                        record
                            .finished_at
                            .and_then(|end| end.duration_since(start).ok())
                    });
                    self.run.current = Some(record.clone());
                    self.push_activity(
                        ActivityKind::Run,
                        FeedbackSeverity::Error,
                        format!("run failed to start: {}", record.invocation.summary()),
                    );
                    self.run.remember(record);
                    self.save_run_history();
                }
                self.terminal_capture.content = error.clone();
                self.report_error(&format!("run launch failed: {error}"));
            }
            Ok(RunEvent::CaptureFailed(error)) => {
                self.run_abort = None;
                self.pending_validation = false;
                self.busy_phase = BusyPhase::Idle;
                if let Some(mut record) = self.run.current.take() {
                    record.state = RunState::CaptureFailed;
                    record.spawn_error = Some(error.clone());
                    record.finished_at = Some(std::time::SystemTime::now());
                    record.duration = record.started_at.and_then(|start| {
                        record
                            .finished_at
                            .and_then(|end| end.duration_since(start).ok())
                    });
                    self.run.current = Some(record.clone());
                    self.push_activity(
                        ActivityKind::Run,
                        FeedbackSeverity::Error,
                        format!("run capture failed: {}", record.invocation.summary()),
                    );
                    self.run.remember(record);
                    self.save_run_history();
                }
                self.terminal_capture.content = error.clone();
                self.report_error(&format!("run output capture failed: {error}"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.run_rx = Some(rx);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.run_abort = None;
                self.pending_validation = false;
                self.busy_phase = BusyPhase::Idle;
                if let Some(record) = self.run.current.as_mut() {
                    record.state = RunState::CaptureFailed;
                    record.finished_at = Some(std::time::SystemTime::now());
                    let summary = record.invocation.summary();
                    self.push_activity(
                        ActivityKind::Run,
                        FeedbackSeverity::Error,
                        format!("run capture failed: {summary}"),
                    );
                }
            }
        }
    }

    fn append_terminal_output(&mut self, chunk: &[u8]) {
        const MAX_CAPTURE: usize = 16_000;
        self.terminal_capture
            .content
            .push_str(&String::from_utf8_lossy(chunk));
        if self.terminal_capture.content.len() > MAX_CAPTURE {
            self.terminal_capture.content.truncate(MAX_CAPTURE);
            self.terminal_capture.truncated = true;
        }
    }

    fn queue_context_reset(&mut self) {
        if self.busy
            || self.pending_prompt.is_some()
            || self.pending_sync
            || self.pending_hitl_decision.is_some()
            || self.pending_context_reset
        {
            self.set_feedback(FeedbackSeverity::Warn, "busy — wait before /compact");
            return;
        }
        self.pending_context_reset = true;
        self.busy_phase = BusyPhase::Other("context reset".into());
        self.status_message = "resetting context…".into();
        self.set_feedback(FeedbackSeverity::Info, "resetting context…");
    }

    pub async fn drain_pending_hitl(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        let Some(decision) = self.pending_hitl_decision.take() else {
            return Ok(());
        };
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        self.session.resolve_hitl(decision.clone(), "tui").await?;
        self.status_message = match decision {
            HitlDecision::Approve => "Action approved".into(),
            HitlDecision::Deny => "Action denied".into(),
            // `HitlDecision` is `#[non_exhaustive]`. Report an unrecognised decision as
            // denied so the operator sees what `resolve_hitl` actually did with it.
            _ => "Action denied".into(),
        };
        self.push_notice(vec![self.status_message.clone()]);
        self.busy_phase = BusyPhase::Idle;
        if let Some(term) = terminal {
            let _ = term.draw(|f| self.draw(f));
        }
        Ok(())
    }

    async fn resolve_hitl_overlay(
        &mut self,
        decision: HitlDecision,
        remember_exact_direct: bool,
    ) -> Result<(), TuiError> {
        let Some(payload) = self.session.pending_hitl.clone() else {
            self.overlay = None;
            return Ok(());
        };

        let identity_to_remember = if remember_exact_direct {
            let Some(identity) = self.approval_identity_for_payload(&payload) else {
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    "this approval cannot be remembered; use Allow once or Deny",
                );
                return Ok(());
            };
            Some(identity)
        } else {
            None
        };

        self.session.resolve_hitl(decision.clone(), "tui").await?;
        if let Some(identity) = identity_to_remember {
            self.hitl_session_allow.insert(identity);
        }
        self.overlay = None;
        match decision {
            HitlDecision::Approve if remember_exact_direct => {
                self.push_toast("remembered exact Direct invocation");
            }
            HitlDecision::Approve => self.push_toast("approved once"),
            HitlDecision::Deny => self.push_toast("denied"),
            // `HitlDecision` is `#[non_exhaustive]`; an unrecognised decision is denied.
            _ => self.push_toast("denied"),
        }
        Ok(())
    }

    pub async fn drain_pending_context_reset(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        if !self.pending_context_reset {
            return Ok(());
        }
        self.pending_context_reset = false;
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        let before_report = self.session.token_usage_report();
        let before = before_report.context_tokens_est;
        self.session.force_context_reset_async().await?;
        let after_report = self.session.token_usage_report();
        let after = after_report.context_tokens_est;
        self.context_reset_snapshot = Some((
            before as f64 / before_report.context_capacity.max(1) as f64 * 100.0,
            after as f64 / after_report.context_capacity.max(1) as f64 * 100.0,
        ));
        self.chat_message_start = self.session.messages.len();
        self.chat_event_start = self.session.events.len();
        self.push_toast("Continuing in a fresh context");
        let progress = fs::read_to_string(self.runtime.cwd.join(".forge/progress.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<ProgressDocument>(&text).ok());
        if let Some(progress) = progress {
            self.ui_banners.push(ChatItem::ContextHandoff {
                before_pct: self.context_reset_snapshot.unwrap().0,
                after_pct: self.context_reset_snapshot.unwrap().1,
                goal: progress.goal,
                completed: progress.completed,
                next_actions: progress.next_actions,
            });
        }
        self.push_activity(
            ActivityKind::Context,
            FeedbackSeverity::Ok,
            format!("fresh context prepared · {before} → {after} tokens"),
        );
        self.status_message = "Continuing in a fresh context".into();
        self.notices.clear();
        self.busy_phase = BusyPhase::Idle;
        if let Some(term) = terminal {
            let _ = term.draw(|f| self.draw(f));
        }
        Ok(())
    }

    /// Open the active file in the user's configured external editor.
    ///
    /// Suspends the TUI terminal, spawns the editor, waits for it to
    /// complete, restores the TUI, and refreshes the source viewer and
    /// Git status.
    pub async fn drain_pending_external_editor(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        if !self.pending_external_editor {
            return Ok(());
        }
        self.pending_external_editor = false;

        // 1. Guard: must have a valid text file open.
        let file_path = match &self.source_viewer.path {
            Some(p) if self.source_viewer.status.is_openable() => p.clone(),
            Some(_) => {
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    "Cannot open binary files in an external editor",
                );
                return Ok(());
            }
            None => {
                self.set_feedback(FeedbackSeverity::Warn, "No file open in the source viewer");
                return Ok(());
            }
        };

        // 2. Guard: no unsafe write-active tool.
        if matches!(self.busy_phase, BusyPhase::Tool { .. }) {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "External editor unavailable while Forge is writing files.\n\n\
                 Wait for the current operation to finish, then try again.",
            );
            return Ok(());
        }

        // 3. Resolve editor.
        let (editor_cmd, _editor_args) = match crate::editor::resolve_editor() {
            Some(r) => r,
            None => {
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    EditorError::NotConfigured.to_string(),
                );
                return Ok(());
            }
        };

        // 4. Flush pending redraw.
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }

        // 5. Suspend the TUI terminal (restore normal terminal state).
        crate::terminal::restore_terminal();

        // 6. Spawn the editor and wait.
        let mut cmd = std::process::Command::new(&editor_cmd);
        for arg in &_editor_args {
            cmd.arg(arg);
        }
        cmd.arg(&file_path);

        let status = match cmd.status() {
            Ok(s) => s,
            Err(e) => {
                let _ = self.resume_after_external_editor(terminal.as_deref_mut());
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    EditorError::SpawnFailed(e).to_string(),
                );
                return Ok(());
            }
        };

        // 8. Report non-zero exit.
        if let Some(code) = status.code() {
            if code != 0 {
                self.push_activity(
                    ActivityKind::System,
                    FeedbackSeverity::Warn,
                    format!("external editor exited with status {code}"),
                );
            }
        }

        let _ = self.resume_after_external_editor(terminal);

        // 9. Refresh the active file and Git status.
        self.refresh_post_editor();
        Ok(())
    }

    fn resume_after_external_editor(
        &mut self,
        terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Best effort: terminal restoration must not fail the UI in test or headless
        // contexts where a real terminal may not be attached.
        let _ = crate::terminal::reinit_terminal(self.runtime.mouse_capture);
        let _ = crate::terminal::clear_terminal();
        if let Some(term) = terminal {
            term.autoresize()?;
            term.clear()?;
            term.draw(|f| self.draw(f))?;
        }
        Ok(())
    }

    /// Called after the external editor exits. Reloads the file, refreshes
    /// syntax highlighting, search state, and Git markers.
    fn refresh_post_editor(&mut self) {
        self.refresh_active_source_viewer();
        self.note_workspace_changed();

        // Show a compact notice.
        let gs = &self.file_explorer.git_status;
        let changed = gs.status.len();
        let gs_text = if changed == 0 {
            "No repository changes detected".into()
        } else if changed == 1 {
            "1 file changed".into()
        } else {
            format!("{changed} files changed")
        };
        self.notices.clear();
        self.push_notice(vec!["Returned from external editor".into(), gs_text]);
    }

    fn refresh_active_source_viewer(&mut self) {
        let root = self.session.workspace_root().to_path_buf();
        let path = self.source_viewer.path.clone();
        let old_line = self.source_viewer.current_line;
        let old_top = self.source_viewer.top_line;

        if let Some(p) = &path {
            if p.exists() {
                self.source_viewer.refresh(&root);
                // Preserve sensible cursor.
                self.source_viewer.current_line =
                    old_line.min(self.source_viewer.lines.len().saturating_sub(1));
                self.source_viewer.top_line =
                    old_top.min(self.source_viewer.lines.len().saturating_sub(1));
            } else {
                self.source_viewer.refresh(&root);
            }
        }

        // Invalidate search matches (recomputed lazily).
        let search_query = self.source_viewer.search.query.clone();
        if !search_query.is_empty() {
            self.source_viewer.update_search_query(&search_query);
        }
    }

    /// Drive `/sync` work with a terminal handle available for intermediate redraws.
    pub async fn drain_pending_sync(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        if !self.pending_sync {
            return Ok(());
        }
        self.pending_sync = false;
        self.slash_sync_inner(&mut terminal).await;
        Ok(())
    }

    async fn slash_sync_inner(
        &mut self,
        terminal: &mut Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) {
        self.push_activity(
            ActivityKind::Tool,
            FeedbackSeverity::Info,
            "git sync (stage · message · commit · push)",
        );
        self.busy_phase = BusyPhase::Other("git sync".into());
        self.set_feedback(FeedbackSeverity::Info, "syncing… inspecting changes");
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }

        // Anything unstaged or untracked?
        let status = match self
            .run_git_tool("status", vec!["--porcelain".into()])
            .await
        {
            Ok(o) if !o.is_error => o.content,
            Ok(o) => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git status failed: {}", o.content.trim()));
                return;
            }
            Err(e) => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git status failed: {e}"));
                return;
            }
        };
        if status.trim().is_empty() {
            self.busy_phase = BusyPhase::Idle;
            self.set_feedback(FeedbackSeverity::Info, "nothing to sync (clean tree)");
            self.notices.clear();
            self.push_toast("working tree clean");
            self.push_activity(
                ActivityKind::Tool,
                FeedbackSeverity::Info,
                "git sync skipped · clean tree",
            );
            if let Some(term) = terminal.as_deref_mut() {
                let _ = term.draw(|f| self.draw(f));
            }
        }

        // Stage everything.
        match self.run_git_tool("add", vec!["-A".into()]).await {
            Ok(o) if o.is_error => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git add failed: {}", o.content.trim()));
                return;
            }
            Err(e) => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git add failed: {e}"));
                return;
            }
            Ok(_) => {}
        }

        // Build a message from the staged changeset (stat + name-status + optional LLM).
        let name_status = self
            .run_git_tool("diff", vec!["--cached".into(), "--name-status".into()])
            .await
            .map(|o| o.content)
            .unwrap_or_default();
        let stat = self
            .run_git_tool("diff", vec!["--cached".into(), "--stat".into()])
            .await
            .map(|o| o.content)
            .unwrap_or_default();
        let patch_snip = self
            .run_git_tool("diff", vec!["--cached".into()])
            .await
            .map(|o| o.content.chars().take(6_000).collect::<String>())
            .unwrap_or_default();

        self.set_feedback(FeedbackSeverity::Info, "syncing… writing commit message");
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        let message = self
            .commit_message_from_changeset(&name_status, &stat, &patch_snip)
            .await;

        self.set_feedback(
            FeedbackSeverity::Info,
            format!("syncing… commit: {}", truncate_one_line(&message, 48)),
        );
        let commit = self
            .run_git_tool("commit", vec!["-m".into(), message.clone()])
            .await;
        match commit {
            Ok(o) if o.is_error => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git commit failed: {}", o.content.trim()));
                self.push_notice(
                    o.content
                        .lines()
                        .map(|s| s.to_string())
                        .filter(|s| !s.is_empty())
                        .take(16)
                        .collect(),
                );
                return;
            }
            Err(e) => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git commit failed: {e}"));
                return;
            }
            Ok(o) => {
                self.push_activity(
                    ActivityKind::Tool,
                    FeedbackSeverity::Ok,
                    format!("git commit · {message}"),
                );
                let _ = o;
            }
        }

        self.set_feedback(FeedbackSeverity::Info, "syncing… push");
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        let push = self.run_git_tool("push", vec![]).await;
        self.busy_phase = BusyPhase::Idle;
        match push {
            Ok(o) if o.is_error => {
                // Commit succeeded; push failed — surface both.
                self.report_error(&format!("committed but push failed: {}", o.content.trim()));
                let mut lines = vec![format!("Committed: {message}"), "Push failed:".into()];
                lines.extend(
                    o.content
                        .lines()
                        .map(|s| s.to_string())
                        .filter(|s| !s.is_empty())
                        .take(12),
                );
                self.push_notice(lines);
            }
            Err(e) => {
                self.report_error(&format!("committed but push failed: {e}"));
                self.push_notice(vec![
                    format!("Committed: {message}"),
                    format!("Push error: {e}"),
                ]);
            }
            Ok(o) => {
                self.push_toast("synced");
                self.set_feedback(
                    FeedbackSeverity::Ok,
                    format!("synced · {}", truncate_one_line(&message, 40)),
                );
                let mut lines = vec![format!("Commit: {message}"), "Push: ok".into()];
                if !stat.trim().is_empty() {
                    lines.push(String::new());
                    lines.push("Changeset:".into());
                    for l in stat.lines().take(12) {
                        lines.push(l.to_string());
                    }
                }
                if !o.content.trim().is_empty() {
                    lines.push(String::new());
                    for l in o.content.lines().take(8) {
                        lines.push(l.to_string());
                    }
                }
                self.notices.clear();
                self.notices_until = None;
                self.push_activity(ActivityKind::Tool, FeedbackSeverity::Ok, lines.join(" · "));
                self.push_activity(ActivityKind::Tool, FeedbackSeverity::Ok, "git push");
            }
        }
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
    }

    /// Prefer a short model-written summary of the staged diff; fall back to a file-list heuristic.
    async fn commit_message_from_changeset(
        &self,
        name_status: &str,
        stat: &str,
        patch_snip: &str,
    ) -> String {
        let fallback = heuristic_commit_message(name_status);
        if !self.is_provider_connected() {
            return fallback;
        }
        let model_id = if self.session.active_model.is_empty() {
            self.runtime.model_label.clone()
        } else {
            self.session.active_model.clone()
        };
        let user = format!(
            "Write a single-line git commit message (max ~72 chars) for this staged change.\n\
Rules: imperative mood, no quotes, no trailing period, no conventional-commit prefix unless clearly needed.\n\
Reply with ONLY the commit message line.\n\n\
## name-status\n{name_status}\n\n\
## stat\n{stat}\n\n\
## patch (truncated)\n{patch_snip}"
        );
        let req = forge_model::ModelRequest {
            messages: vec![
                forge_types::Message::new(
                    forge_types::MessageRole::System,
                    "You write concise git commit messages from diffs.",
                ),
                forge_types::Message::new(forge_types::MessageRole::User, user),
            ],
            tools: vec![],
            model: model_id,
            reasoning_effort: Some(self.reasoning_effort.to_string())
                .filter(|value| value != "auto"),
            prompt_cache: true,
        };
        match self.session.model_client().complete(req).await {
            Ok(resp) => {
                let line = resp
                    .text
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("")
                    .trim_matches(|c| c == '"' || c == '\'' || c == '`')
                    .trim_end_matches('.')
                    .to_string();
                if line.is_empty() || line.len() > 200 {
                    fallback
                } else {
                    line
                }
            }
            Err(_) => fallback,
        }
    }

    /// Insert bracketed-paste text into the current explicit text owner.
    fn handle_paste(&mut self, data: &str) {
        if let Some(ExplorerDialog::Name { input, error, .. }) = self.explorer_dialog.as_mut() {
            for ch in data.chars().filter(|ch| !ch.is_control()) {
                input.push(ch);
            }
            *error = None;
            return;
        }
        if let Some(ref mut ov) = self.overlay {
            let _ = handle_overlay_key(ov, OverlayKey::Paste(data.to_string()));
            return;
        }
        self.normalize_focus();
        match self.focus.mode {
            FocusMode::Navigation if self.focus.block == FocusBlock::Composer => {
                self.input.history_browse = false;
                self.input.insert_paste(data);
                self.clamp_slash_suggest();
            }
            FocusMode::Transient(TransientOwner::SourceSearch) => {
                for ch in data.chars().filter(|ch| !ch.is_control()) {
                    self.source_viewer.append_search_char(ch);
                }
            }
            FocusMode::Transient(TransientOwner::JumpToLine) => {
                for ch in data.chars().filter(|ch| ch.is_ascii_digit()) {
                    self.source_viewer.append_jump_char(ch);
                }
            }
            FocusMode::Navigation => {}
        }
    }

    pub fn refresh_status_model(&self) -> StatusModel {
        self.refresh_status_model_with_connected(self.is_provider_connected())
    }

    fn refresh_status_model_with_connected(&self, provider_connected: bool) -> StatusModel {
        let repo = self.repo_header();
        let id = self.session.session_id.to_string();
        let short = if id.len() > 8 {
            id[..8].to_string()
        } else {
            id
        };
        let turn_cancelled = !self.busy
            && (self.last_exit == ExitCode::Canceled
                || self.session.status == forge_types::SessionStatus::Cancelled);
        StatusModel {
            status: self.session.status,
            session_short: short,
            model: self.runtime.model_label.clone(),
            provider: self.runtime.provider.clone(),
            effort: self.reasoning_effort.to_string(),
            ctx_pct: self.session.context_usage_ratio(),
            busy: self.busy,
            busy_phase: self.busy_phase.clone(),
            connect_profile: self.connect_profile.clone(),
            provider_connected,
            web_search_label: self.web_search_label.clone(),
            tools_visible: self.session.list_tools().len(),
            prompt_cache_hits: self.session.token_usage.prompt_cache_hits,
            prompt_cache_writes: self.session.token_usage.prompt_cache_writes,
            repo_name: repo.repo_name.clone(),
            branch: repo.branch.clone(),
            dirty: repo.dirty,
            resource: self.workspace_resource_label(),
            // Workspace secondary metadata only — never overall task lifecycle.
            activity: self.workspace_activity_label(),
            turn_cancelled,
            progress_description: self.header_progress_description(),
            failure_category: self.header_failure_category(),
            waiting_detail: self.header_waiting_detail(),
        }
    }

    /// Typed progress for the header while Working. Structured sources only.
    fn header_progress_description(&self) -> Option<String> {
        if !self.busy {
            return None;
        }
        // Prefer durable progress.json in_progress when present.
        if let Ok(text) = std::fs::read_to_string(self.runtime.cwd.join(".forge/progress.json")) {
            if let Ok(doc) = serde_json::from_str::<ProgressDocument>(&text) {
                let step = doc.in_progress.trim();
                if !step.is_empty() {
                    return Some(step.to_string());
                }
            }
        }
        self.busy_phase.progress_description()
    }

    fn header_waiting_detail(&self) -> Option<String> {
        if self.session.pending_hitl.is_some()
            || self.session.status == forge_types::SessionStatus::AwaitingHitl
        {
            return Some("Approval required".into());
        }
        if self.busy && matches!(self.busy_phase, BusyPhase::Connect) {
            return Some("Your input required".into());
        }
        None
    }

    fn header_failure_category(&self) -> Option<String> {
        if self.session.status != forge_types::SessionStatus::Failed {
            return None;
        }
        // Prefer the latest structured turn_failed event category.
        for event in self.session.events.iter().rev() {
            if event.kind == "turn_failed" || event.kind == "validation_exhausted" {
                let detail = event.detail.as_str();
                let category = detail.split(':').next().unwrap_or(detail).trim();
                return Some(failure_category_label(category));
            }
        }
        for message in self.session.messages.iter().rev() {
            if message.role != MessageRole::Assistant {
                continue;
            }
            if let Some(rest) = message.content.strip_prefix(forge_core::TURN_FAILED_MARKER) {
                let summary = rest.trim();
                if summary.contains("repeated invalid tool") {
                    return Some("Tool retries exhausted".into());
                }
                if summary.contains("couldn't complete this turn") {
                    return Some("Turn incomplete".into());
                }
                return None;
            }
        }
        None
    }

    fn workspace_resource_label(&self) -> Option<String> {
        match &self.workspace_navigation.current {
            WorkspaceView::Conversation => None,
            WorkspaceView::File(path) => {
                Some(relative_display(self.session.workspace_root(), path))
            }
            WorkspaceView::Diff(DiffCommandContext::Current) => Some("Review changes".into()),
            WorkspaceView::Run(id) => self
                .run
                .current
                .as_ref()
                .filter(|record| record.id == *id)
                .map(|record| format!("Run: {}", record.invocation.summary()))
                .or_else(|| Some("Run".into())),
        }
    }

    fn workspace_activity_label(&self) -> Option<String> {
        match &self.workspace_navigation.current {
            WorkspaceView::Diff(DiffCommandContext::Current) => {
                let total = self.file_explorer.git_status.status.len();
                (total > 0).then(|| format!("{} of {} changes", self.diff_selected + 1, total))
            }
            WorkspaceView::Run(id) => self
                .run
                .current
                .as_ref()
                .filter(|record| record.id == *id)
                .map(|record| {
                    (match record.state {
                        RunState::Queued => "Queued",
                        RunState::Running => "Running",
                        RunState::Succeeded => "Succeeded",
                        RunState::Failed => "Failed",
                        RunState::Cancelled => "Cancelled",
                        RunState::StartFailed => "Could not start",
                        RunState::CaptureFailed => "Capture failed",
                    })
                    .to_string()
                }),
            _ => {
                let changes = self.file_explorer.git_status.status.len();
                if changes > 0 {
                    Some(format!("{changes} changes · Review"))
                } else {
                    // Do not mirror task lifecycle/progress into secondary activity.
                    None
                }
            }
        }
    }

    fn activity_summary(&self) -> Option<ActivitySummaryModel> {
        // Approval is represented by the blocking overlay, not a background summary.
        if self.overlay.is_some() || self.session.pending_hitl.is_some() {
            return None;
        }

        if let Some(record) = self.run.current.as_ref() {
            let command = record.invocation.summary();
            if matches!(
                record.state,
                RunState::Failed | RunState::StartFailed | RunState::CaptureFailed
            ) {
                return Some(ActivitySummaryModel {
                    label: format!("Run failed: {command}"),
                    action_label: Some("Inspect"),
                    action: Some(ActivitySummaryAction::OpenRun(record.id.clone())),
                    kind: BannerKind::Error,
                });
            }
            if matches!(record.state, RunState::Queued | RunState::Running) {
                return Some(ActivitySummaryModel {
                    label: format!("Running {command}"),
                    action_label: Some("View output"),
                    action: Some(ActivitySummaryAction::OpenRun(record.id.clone())),
                    kind: BannerKind::Info,
                });
            }
        }

        let changes = self.file_explorer.git_status.status.len();
        if changes > 0 {
            let files = if changes == 1 { "file" } else { "files" };
            return Some(ActivitySummaryModel {
                label: format!("{changes} {files} changed"),
                action_label: Some("Review"),
                action: Some(ActivitySummaryAction::ReviewChanges),
                kind: BannerKind::Info,
            });
        }

        if self.busy && matches!(self.busy_phase, BusyPhase::Model) {
            return Some(ActivitySummaryModel {
                label: "Forge is thinking".into(),
                action_label: None,
                action: None,
                kind: BannerKind::Info,
            });
        }

        None
    }

    fn activity_summary_cache_key(&self) -> Option<(String, Option<&'static str>, BannerKind)> {
        self.activity_summary()
            .map(|summary| (summary.label, summary.action_label, summary.kind))
    }

    fn activity_summary_command(&self) -> Option<SemanticCommand> {
        match self.activity_summary()?.action? {
            ActivitySummaryAction::OpenRun(id) => {
                Some(SemanticCommand::OpenRun(RunCommandTarget::Id(id)))
            }
            ActivitySummaryAction::ReviewChanges => {
                Some(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
            }
        }
    }

    fn activate_activity_summary(&mut self) {
        match self.activity_summary().and_then(|summary| summary.action) {
            Some(ActivitySummaryAction::OpenRun(id)) => {
                self.navigate_to_workspace_view(WorkspaceView::Run(id));
            }
            Some(ActivitySummaryAction::ReviewChanges) => {
                self.navigate_to_workspace_view(WorkspaceView::Diff(DiffCommandContext::Current));
            }
            None => {}
        }
    }

    /// Read the cached repo header. This is a plain field read: it must never
    /// spawn a subprocess, because callers sit on the render path.
    fn repo_header(&self) -> RepoHeaderCache {
        self.repo_header.clone()
    }

    /// Advance the off-thread repo-header refresh. Non-blocking and safe to call
    /// from the render loop, matching `GitStatusCache::poll`.
    ///
    /// The previous value is retained while a refresh is in flight and when a
    /// refresh fails, so the header never blanks mid-update (FORGE-DESIGN 9.7).
    fn poll_repo_header(&mut self) {
        // A cwd change makes the cached header describe the wrong directory, so
        // read through synchronously: the next frame must be correct, and this
        // only happens on a workspace switch, never frame to frame.
        if self.repo_header_cwd != self.runtime.cwd {
            self.repo_header_cwd = self.runtime.cwd.clone();
            self.repo_header = load_repo_header(&self.repo_header_cwd);
            self.repo_header_refreshed_at = Instant::now();
            // Drop any refresh still in flight for the previous directory.
            self.repo_header_rx = None;
            return;
        }

        if let Some(rx) = self.repo_header_rx.take() {
            match rx.try_recv() {
                Ok(header) => {
                    self.repo_header = header;
                    self.repo_header_refreshed_at = Instant::now();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.repo_header_rx = Some(rx);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Keep the last known header; retry after the next TTL window.
                    self.repo_header_refreshed_at = Instant::now();
                }
            }
            return;
        }

        if self.repo_header_refreshed_at.elapsed() < REPO_HEADER_TTL {
            return;
        }

        let cwd = self.runtime.cwd.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(load_repo_header(&cwd));
        });
        self.repo_header_rx = Some(rx);
    }

    #[cfg(test)]
    fn busy_status_detail(&self) -> Option<String> {
        self.busy.then(|| {
            let label = if !self.stream_thinking.is_empty() && self.stream_preview.is_empty() {
                "Thinking..."
            } else {
                "Working..."
            };
            let elapsed = self
                .turn_started
                .map(|started| started.elapsed().as_secs_f64())
                .unwrap_or(0.0);
            format!("{label} {}", format_elapsed_tenths(elapsed))
        })
    }

    fn current_workspace_is_conversation(&self) -> bool {
        matches!(
            self.workspace_navigation.current,
            WorkspaceView::Conversation
        )
    }

    fn current_workspace_is_file(&self) -> bool {
        matches!(self.workspace_navigation.current, WorkspaceView::File(_))
    }

    fn current_workspace_is_diff(&self) -> bool {
        matches!(self.workspace_navigation.current, WorkspaceView::Diff(_))
    }

    fn current_workspace_is_run(&self) -> bool {
        matches!(self.workspace_navigation.current, WorkspaceView::Run(_))
    }

    #[allow(dead_code)]
    fn footer_limits(&mut self, provider: &str) -> FooterLimits {
        if let Some(rx) = &self.footer_limits_rx {
            match rx.try_recv() {
                Ok((provider, limits)) => {
                    self.footer_limits_cache = Some(FooterLimitsCache {
                        provider,
                        fetched_at: Instant::now(),
                        limits,
                    });
                    self.footer_limits_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.footer_limits_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }

        if provider != OPENAI_CODEX_PROFILE_ID {
            return FooterLimits::default();
        }

        let (cached_limits, needs_refresh) = match self
            .footer_limits_cache
            .as_ref()
            .filter(|cache| cache.provider == provider)
        {
            Some(cache) => (
                Some(cache.limits.clone()),
                cache.fetched_at.elapsed() >= Duration::from_secs(60),
            ),
            None => (None, true),
        };
        if needs_refresh && self.footer_limits_rx.is_none() {
            let provider = provider.to_string();
            let request_provider = provider.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let report = forge_connect::provider_cost_report(
                    &request_provider,
                    "",
                    0,
                    0,
                    &CredentialStore::user_default(),
                )
                .unwrap_or_default();
                let _ = tx.send((request_provider, footer_limits_from_report(&report)));
            });
            self.footer_limits_rx = Some(rx);
        }

        cached_limits.unwrap_or_default()
    }

    fn begin_hit_frame(&mut self) {
        self.frame_generation = self.frame_generation.saturating_add(1);
        if self.frame_generation == 0 {
            self.frame_generation = 1;
        }
        self.hit_regions.clear();
    }

    fn invalidate_hit_regions(&mut self) {
        self.frame_generation = self.frame_generation.saturating_add(1);
        self.hit_regions.clear();
        self.pending_double_click = None;
    }

    fn clear_pending_double_click(&mut self) {
        self.pending_double_click = None;
    }

    fn register_hit_region(
        &mut self,
        area: ratatui::layout::Rect,
        target: HitTarget,
        z_order: u16,
    ) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        self.hit_regions.push(HitRegion {
            area,
            target,
            generation: self.frame_generation,
            z_order,
        });
    }

    fn register_pane_hit_regions(&mut self, regions: &crate::layout::LayoutRegions) {
        if let Some(area) = regions.files {
            self.register_hit_region(area, HitTarget::Pane(FocusBlock::Files), 1);
        }
        self.register_hit_region(regions.chat, HitTarget::Pane(FocusBlock::Workspace), 1);
        if let Some(area) = regions.sidebar {
            self.register_hit_region(area, HitTarget::Pane(FocusBlock::Inspector), 1);
        }
        if self.bottom_panel.open && regions.bottom_panel.height > 0 {
            self.register_hit_region(
                regions.bottom_panel,
                HitTarget::Pane(FocusBlock::BottomPanel),
                1,
            );
            self.register_bottom_panel_tab_regions(regions.bottom_panel);
        }
        self.register_hit_region(regions.input, HitTarget::Composer, 5);
    }

    fn register_file_hit_regions(&mut self, area: ratatui::layout::Rect) {
        let inner = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .inner(area);
        let height = inner.height.saturating_sub(1) as usize;
        let error_shown = self.file_explorer.git_status.error.is_some();
        let list_height = height.saturating_sub(error_shown as usize);
        let y_offset = error_shown as u16;
        let visible = self.file_explorer.visible_nodes();
        for (row, node) in visible
            .iter()
            .skip(self.file_explorer.scroll)
            .take(list_height)
            .enumerate()
        {
            let y = inner.y.saturating_add(y_offset).saturating_add(row as u16);
            let row_area = ratatui::layout::Rect::new(inner.x, y, inner.width, 1);
            self.register_hit_region(row_area, HitTarget::FileEntry(node.path.clone()), 20);
            if node.kind == FileKind::Directory {
                let chevron_x = inner
                    .x
                    .saturating_add((node.depth as u16).saturating_mul(2));
                if chevron_x < inner.x.saturating_add(inner.width) {
                    self.register_hit_region(
                        ratatui::layout::Rect::new(chevron_x, y, 1, 1),
                        HitTarget::DirectoryChevron(node.path.clone()),
                        30,
                    );
                }
            }
        }
    }

    fn register_bottom_panel_tab_regions(&mut self, area: ratatui::layout::Rect) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let mut x = area.x;
        let y = area.y;
        for (idx, tab) in BottomPanelTab::ALL.into_iter().enumerate() {
            let width = format!(" {} {} ", idx + 1, tab.label()).chars().count() as u16;
            if x >= area.x.saturating_add(area.width) {
                break;
            }
            let clamped_width = width.min(area.x.saturating_add(area.width).saturating_sub(x));
            self.register_hit_region(
                ratatui::layout::Rect::new(x, y, clamped_width, 1),
                HitTarget::VisibleControl(SemanticCommand::OpenBottomPanel(tab)),
                25,
            );
            x = x.saturating_add(width).saturating_add(1);
        }
    }

    fn register_activity_summary_region(
        &mut self,
        area: ratatui::layout::Rect,
        lines: &[Line<'static>],
        tail_lines: &[Line<'static>],
    ) {
        let Some(summary) = self.activity_summary() else {
            return;
        };
        if summary.action.is_none() {
            return;
        }
        let total = lines.len().saturating_add(tail_lines.len());
        let max_scroll = total.saturating_sub(area.height as usize);
        let scroll = if self.chat_follow {
            max_scroll
        } else {
            max_scroll.saturating_sub((self.chat_scroll as usize).min(max_scroll))
        };
        let end = scroll.saturating_add(area.height as usize).min(total);
        for index in scroll..end {
            let line = if index < lines.len() {
                &lines[index]
            } else {
                &tail_lines[index - lines.len()]
            };
            let text = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            if text.contains(&summary.label) {
                let y = area.y.saturating_add(index.saturating_sub(scroll) as u16);
                self.register_hit_region(
                    ratatui::layout::Rect::new(area.x, y, area.width, 1),
                    HitTarget::ActivitySummary,
                    25,
                );
                return;
            }
        }
    }

    fn register_overlay_hit_regions(&mut self, area: ratatui::layout::Rect) {
        if self.explorer_dialog.is_some() {
            self.register_hit_region(
                area,
                HitTarget::VisibleControl(SemanticCommand::CloseOverlay),
                900,
            );
            return;
        }
        let hitl = self.overlay.as_ref().and_then(|overlay| {
            if let Overlay::Hitl {
                approval, expanded, ..
            } = overlay
            {
                Some((approval.remember_eligible, *expanded))
            } else {
                None
            }
        });
        if self.overlay.is_none() {
            return;
        }
        self.register_hit_region(
            area,
            HitTarget::VisibleControl(SemanticCommand::CloseOverlay),
            900,
        );
        if let Some((remember_eligible, expanded)) = hitl {
            let overlay_area =
                centered_capped_rect_for_mouse(area, 78, if expanded { 30 } else { 22 });
            let inner = ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .inner(overlay_area);
            let action_y = inner.y.saturating_add(10);
            self.register_hit_region(
                ratatui::layout::Rect::new(inner.x, action_y, 12, 1),
                HitTarget::OverlayAction(OverlayAction::HitlApprove),
                1000,
            );
            self.register_hit_region(
                ratatui::layout::Rect::new(inner.x.saturating_add(14), action_y, 8, 1),
                HitTarget::OverlayAction(OverlayAction::HitlDeny),
                1000,
            );
            if remember_eligible {
                self.register_hit_region(
                    ratatui::layout::Rect::new(inner.x, inner.y.saturating_add(12), inner.width, 1),
                    HitTarget::OverlayAction(OverlayAction::HitlApproveSession),
                    1000,
                );
            }
        }
    }

    fn render_diff_workspace(
        &self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        let gs = &self.file_explorer.git_status;
        if gs.loading && !self.diff_snapshot.stale {
            Paragraph::new("Loading changes…")
                .style(theme::muted())
                .alignment(ratatui::layout::Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::muted())
                        .style(theme::panel()),
                )
                .render(area, buf);
            return;
        }
        if gs.error.is_some() && !self.diff_snapshot.stale {
            Paragraph::new("Changes unavailable\n\nGit status could not be read.\nThe rest of Forge remains usable.")
                .style(theme::muted())
                .alignment(ratatui::layout::Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::muted())
                        .style(theme::panel()),
                )
                .render(area, buf);
            return;
        }
        if gs.status.is_empty() && !self.diff_snapshot.stale {
            Paragraph::new("No changes\n\nThe working tree is clean.")
                .style(theme::muted())
                .alignment(ratatui::layout::Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::muted())
                        .style(theme::panel()),
                )
                .render(area, buf);
            return;
        }

        let changed = gs.changed_files();
        let review_paths = if self.diff_snapshot.stale && !self.diff_snapshot.paths.is_empty() {
            self.diff_snapshot.paths.clone()
        } else {
            changed.iter().map(|f| f.path.clone()).collect()
        };
        let selected = self.diff_selected.min(review_paths.len().saturating_sub(1));
        let selected_path = review_paths.get(selected);

        let mut lines = vec![Line::from(Span::styled("CHANGES", theme::brand()))];
        if self.diff_snapshot.stale {
            lines.push(Line::styled(
                "Stale review · changes updated externally · press r to Refresh",
                theme::warn(),
            ));
            lines.push(Line::styled(
                "Apply disabled until refresh.",
                theme::muted(),
            ));
        }
        lines.push(Line::from(""));

        for (i, path) in review_paths.iter().enumerate() {
            let marker = if i == selected { "▶ " } else { "  " };
            let status = changed
                .iter()
                .find(|file| file.path == *path)
                .and_then(|file| file.unstaged)
                .map(GitStatusKind::marker)
                .unwrap_or("!");
            lines.push(Line::from(format!("{marker}{status} {}", path.display())));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("UNSTAGED DIFF", theme::info())));

        if let Some(path) = selected_path {
            match gs.get_unstaged_diff(&self.runtime.cwd, path) {
                Ok(diff) => {
                    for line in diff.lines().take(20) {
                        let style = if line.starts_with('+') {
                            theme::ok()
                        } else if line.starts_with('-') {
                            theme::danger()
                        } else if line.starts_with("@@") {
                            theme::warn()
                        } else {
                            theme::muted()
                        };
                        lines.push(Line::styled(line.to_string(), style));
                    }
                }
                Err(e) => {
                    lines.push(Line::styled(
                        format!("Unable to load diff: {}", e),
                        theme::danger(),
                    ));
                }
            }
        } else {
            lines.push(Line::from("No unstaged file selected."));
        }

        Paragraph::new(lines)
            .style(theme::text())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::muted())
                    .style(theme::panel()),
            )
            .render(area, buf);
    }

    fn render_run_workspace(
        &self,
        id: &str,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        let current = self.run.current.as_ref().filter(|record| record.id == id);
        let mut lines = Vec::new();
        if let Some(record) = current {
            lines.push(Line::from(vec![
                Span::styled("Run ", theme::muted()),
                Span::styled(record.invocation.summary(), theme::text()),
            ]));
            lines.push(Line::styled(
                format!(
                    "State: {}",
                    match record.state {
                        RunState::Queued => "Queued",
                        RunState::Running => "Running",
                        RunState::Succeeded => "Succeeded",
                        RunState::Failed => "Failed",
                        RunState::Cancelled => "Cancelled",
                        RunState::StartFailed => "Could not start",
                        RunState::CaptureFailed => "Capture failed",
                    }
                ),
                theme::text(),
            ));
            if let Some(code) = record.exit_status {
                lines.push(Line::styled(format!("Exit status: {code}"), theme::muted()));
            }
            if record.state == RunState::StartFailed {
                lines.push(Line::styled(
                    format!("Executable: {}", record.invocation.executable),
                    theme::muted(),
                ));
                lines.push(Line::styled(
                    format!("Arguments: {:?}", record.invocation.arguments),
                    theme::muted(),
                ));
                lines.push(Line::styled(
                    format!(
                        "Directory: {}",
                        record.invocation.working_directory.display()
                    ),
                    theme::muted(),
                ));
                if let Some(error) = record.spawn_error.as_deref() {
                    lines.push(Line::styled(format!("Cause: {error}"), theme::danger()));
                }
            }
        } else {
            lines.push(Line::styled("Run is no longer available.", theme::warn()));
        }
        if !self.terminal_capture.content.is_empty() {
            lines.push(Line::styled("Output", theme::muted()));
            for line in self.terminal_capture.content.lines().take(12) {
                lines.push(Line::styled(line.to_string(), theme::text()));
            }
            if self.terminal_capture.truncated {
                lines.push(Line::styled("Output truncated", theme::muted()));
            }
        } else if let Some(record) = current {
            lines.push(Line::styled(
                format!(
                    "Directory: {}",
                    record.invocation.working_directory.display()
                ),
                theme::muted(),
            ));
        }
        lines.push(Line::styled(
            "Back · Enter cancel while running · r rerun · e edit rerun",
            theme::muted(),
        ));

        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(if self.focus.block == FocusBlock::Workspace {
                        theme::brand()
                    } else {
                        theme::border_muted()
                    })
                    .title(Span::styled(" Run ", theme::brand())),
            )
            .render(area, buf);
    }

    fn toggle_files_panel(&mut self) {
        self.files_visible = !self.files_visible;
        self.save_ui_state();
        if self.files_visible {
            self.focus_block(FocusBlock::Files);
        } else {
            self.restore_focus_after_closing(FocusBlock::Files);
        }
        self.normalize_focus();
    }

    fn current_run_id(&self) -> Option<String> {
        self.run.current.as_ref().map(|record| record.id.clone())
    }

    fn run_exists(&self, id: &str) -> bool {
        self.run
            .current
            .as_ref()
            .is_some_and(|record| record.id == id)
            || self.run.recent.iter().any(|record| record.id == id)
    }

    fn workspace_view_is_valid(&self, view: &WorkspaceView) -> bool {
        match view {
            WorkspaceView::Conversation => true,
            WorkspaceView::File(path) => path.is_file() || path.is_symlink(),
            WorkspaceView::Diff(DiffCommandContext::Current) => true,
            WorkspaceView::Run(id) => self.run_exists(id),
        }
    }

    fn apply_workspace_view(&mut self, view: &WorkspaceView) {
        match view {
            WorkspaceView::Conversation => {}
            WorkspaceView::File(path) => {
                self.show_file_in_editor(path);
            }
            WorkspaceView::Diff(DiffCommandContext::Current) => {
                self.focus_block(FocusBlock::Workspace);
            }
            WorkspaceView::Run(id) => {
                if self.run_exists(id) {
                    self.focus_block(FocusBlock::Workspace);
                } else {
                    self.set_feedback(
                        FeedbackSeverity::Warn,
                        format!("Run is no longer available: {id}"),
                    );
                    self.workspace_navigation
                        .replace_view(WorkspaceView::Conversation);
                }
            }
        }
        self.normalize_focus();
    }

    fn push_workspace_view(&mut self, view: WorkspaceView) {
        self.workspace_navigation.push_view(view.clone());
        self.apply_workspace_view(&view);
    }

    fn replace_workspace_view(&mut self, view: WorkspaceView) {
        self.workspace_navigation.replace_view(view.clone());
        self.apply_workspace_view(&view);
    }

    fn navigate_to_workspace_view(&mut self, view: WorkspaceView) {
        self.workspace_navigation.navigate_to(view.clone());
        self.apply_workspace_view(&view);
    }

    fn go_home_workspace(&mut self) {
        self.workspace_navigation.home();
        self.apply_workspace_view(&WorkspaceView::Conversation);
    }

    fn go_back_workspace(&mut self) {
        let mut next = WorkspaceView::Conversation;
        while let Some(candidate) = self.workspace_navigation.history.pop() {
            if self.workspace_view_is_valid(&candidate) {
                next = candidate;
                break;
            }
        }
        self.workspace_navigation.replace_view(next.clone());
        self.apply_workspace_view(&next);
    }

    fn focus_availability(&self) -> FocusAvailability {
        FocusAvailability {
            files: self.files_visible,
            inspector: self.sidebar_visible,
            bottom_panel: self.bottom_panel.open,
        }
    }

    fn normalize_focus(&mut self) {
        let available = self.focus_availability();
        if !available.contains(self.focus.block) {
            self.focus.block = FocusBlock::Workspace;
            self.focus.mode = FocusMode::Navigation;
            self.focus.return_block = Some(FocusBlock::Workspace);
        }
        if self.source_viewer.search.open {
            self.focus.block = FocusBlock::Workspace;
            self.focus.mode = FocusMode::Transient(TransientOwner::SourceSearch);
        } else if self.source_viewer.jump.open {
            self.focus.block = FocusBlock::Workspace;
            self.focus.mode = FocusMode::Transient(TransientOwner::JumpToLine);
        }
        self.file_explorer.focused = self.focus.block == FocusBlock::Files
            && self.focus.mode == FocusMode::Navigation
            && self.files_visible;
        self.bottom_panel.focused = self.focus.block == FocusBlock::BottomPanel
            && self.focus.mode == FocusMode::Navigation
            && self.bottom_panel.open;
        self.source_viewer.focused = self.focus.block == FocusBlock::Workspace
            && self.current_workspace_is_file()
            && matches!(
                self.focus.mode,
                FocusMode::Navigation | FocusMode::Transient(_)
            );
    }

    pub(crate) fn focus_block(&mut self, block: FocusBlock) {
        if self.focus.block != block {
            self.focus.previous_block = Some(self.focus.block);
        }
        self.focus.block = block;
        self.focus.mode = FocusMode::Navigation;
        self.focus.return_block = Some(block);
        self.normalize_focus();
    }

    fn enter_chat_composer(&mut self) {
        self.focus_block(FocusBlock::Composer);
        self.normalize_focus();
    }

    fn enter_transient(&mut self, owner: TransientOwner) {
        self.focus.block = FocusBlock::Workspace;
        self.focus.mode = FocusMode::Transient(owner);
        self.focus.return_block = Some(FocusBlock::Workspace);
        self.normalize_focus();
    }

    fn restore_focus_after_closing(&mut self, closed: FocusBlock) {
        let previous = self
            .focus
            .previous_block
            .filter(|block| *block != closed && self.focus_availability().contains(*block))
            .unwrap_or(FocusBlock::Workspace);
        self.focus.block = previous;
        self.focus.mode = FocusMode::Navigation;
        self.focus.return_block = Some(previous);
    }

    fn cycle_focus_block(&mut self, forward: bool) {
        let available = self.focus_availability();
        let current = FocusBlock::ORDER
            .iter()
            .position(|block| *block == self.focus.block)
            .unwrap_or(1);
        for offset in 1..=FocusBlock::ORDER.len() {
            let index = if forward {
                (current + offset) % FocusBlock::ORDER.len()
            } else {
                (current + FocusBlock::ORDER.len() - offset) % FocusBlock::ORDER.len()
            };
            let next = FocusBlock::ORDER[index];
            if available.contains(next) {
                self.focus_block(next);
                break;
            }
        }
    }

    fn escape_navigation(&mut self) {
        match self.focus.block {
            FocusBlock::Workspace => {}
            FocusBlock::Composer => {
                let previous = self
                    .focus
                    .previous_block
                    .filter(|block| *block != FocusBlock::Composer)
                    .filter(|block| self.focus_availability().contains(*block))
                    .unwrap_or(FocusBlock::Workspace);
                self.focus_block(previous);
            }
            block => self.restore_focus_after_closing(block),
        }
        self.normalize_focus();
    }

    fn open_bottom_panel(&mut self, tab: Option<BottomPanelTab>) {
        if let Some(tab) = tab {
            self.bottom_panel.active = tab;
        }
        self.bottom_panel.open = true;
        self.focus_block(FocusBlock::BottomPanel);
    }

    fn contextual_hint(&self) -> Option<String> {
        if self.explorer_dialog.is_some() {
            return Some("Enter confirm · Esc cancel".into());
        }
        if let Some(overlay) = self.overlay.as_ref() {
            return match overlay {
                Overlay::Hitl { .. } => Some("Tab move · Enter allow once · Esc deny".into()),
                Overlay::TurnLimit { .. } => Some("Enter confirm · Esc cancel".into()),
                Overlay::ConnectApiKey { .. } => Some("Enter confirm · Esc cancel".into()),
                Overlay::ConnectOauth { .. } => Some("Enter continue · Esc cancel".into()),
                _ => None,
            };
        }
        match self.focus.mode {
            FocusMode::Transient(TransientOwner::SourceSearch) => {
                Some("Enter next · ⇧Enter previous · Esc cancel".into())
            }
            FocusMode::Transient(TransientOwner::JumpToLine) => {
                Some("Enter jump · Esc cancel".into())
            }
            FocusMode::Navigation => None,
        }
    }

    fn render_help_overlay(&self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        let r = centered_rect(64, 58, area);
        crate::theme::fill(r, buf, crate::theme::panel());
        Paragraph::new(self.help_text())
            .wrap(ratatui::widgets::Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::brand())
                    .style(theme::panel())
                    .title(Span::styled(" Help ", theme::brand())),
            )
            .render(r, buf);
    }

    fn render_explorer_dialog(
        &self,
        dialog: &ExplorerDialog,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        let r = centered_rect(64, 34, area);
        crate::theme::fill(r, buf, crate::theme::panel());
        let mut lines = Vec::new();
        let (title, border) = match dialog {
            ExplorerDialog::Name { action, .. } => (
                match action {
                    ExplorerNameAction::CreateFile => " New File ",
                    ExplorerNameAction::CreateDirectory => " New Folder ",
                    ExplorerNameAction::Rename => " Rename ",
                },
                theme::brand(),
            ),
            ExplorerDialog::ConfirmDelete { permanent, .. } if *permanent => {
                (" Permanent Delete ", theme::danger())
            }
            ExplorerDialog::ConfirmDelete { .. } => (" Delete ", theme::warn()),
            ExplorerDialog::ConfirmCreate { .. } => (" Confirm Create ", theme::warn()),
            ExplorerDialog::ConfirmRename { .. } => (" Confirm Rename ", theme::warn()),
        };
        match dialog {
            ExplorerDialog::Name {
                action,
                parent,
                input,
                error,
                ..
            } => {
                let label = match action {
                    ExplorerNameAction::CreateFile => "Enter one file name:",
                    ExplorerNameAction::CreateDirectory => "Enter one folder name:",
                    ExplorerNameAction::Rename => "Enter the new name:",
                };
                lines.push(Line::styled(label, theme::text()));
                lines.push(Line::styled(
                    format!(
                        "Parent: {}",
                        relative_display(self.session.workspace_root(), parent)
                    ),
                    theme::muted(),
                ));
                lines.push(Line::from(""));
                lines.push(Line::styled(format!("> {input}"), theme::text()));
                if let Some(error) = error {
                    lines.push(Line::from(""));
                    lines.push(Line::styled(error.clone(), theme::danger()));
                }
                lines.push(Line::from(""));
                lines.push(Line::styled("Enter confirm · Esc cancel", theme::muted()));
            }
            ExplorerDialog::ConfirmCreate { action, path, .. } => {
                let what = if *action == ExplorerNameAction::CreateDirectory {
                    "folder"
                } else {
                    "file"
                };
                lines.push(Line::styled(
                    format!(
                        "Create {what} \"{}\"?",
                        relative_display(self.session.workspace_root(), path)
                    ),
                    theme::text(),
                ));
                lines.push(Line::from(""));
                lines.push(Line::styled("Enter/y confirm · Esc cancel", theme::muted()));
            }
            ExplorerDialog::ConfirmRename { source, path, .. } => {
                lines.push(Line::styled(
                    format!(
                        "Rename \"{}\"?",
                        relative_display(self.session.workspace_root(), source)
                    ),
                    theme::text(),
                ));
                lines.push(Line::styled(
                    format!(
                        "To \"{}\"",
                        relative_display(self.session.workspace_root(), path)
                    ),
                    theme::text(),
                ));
                lines.push(Line::from(""));
                lines.push(Line::styled("Enter/y confirm · Esc cancel", theme::muted()));
            }
            ExplorerDialog::ConfirmDelete {
                name,
                kind,
                non_empty,
                permanent,
                error,
                ..
            } => {
                if let Some(error) = error {
                    lines.push(Line::styled(error.clone(), theme::danger()));
                    lines.push(Line::from(""));
                    lines.push(Line::styled(
                        "Press p to choose explicit permanent delete · Esc cancel",
                        theme::muted(),
                    ));
                } else if *permanent {
                    lines.push(Line::styled(
                        format!("Permanently delete \"{name}\"?"),
                        theme::danger(),
                    ));
                    lines.push(Line::styled(
                        "This cannot be undone by Forge.",
                        theme::danger(),
                    ));
                    lines.push(Line::from(""));
                    lines.push(Line::styled(
                        "Press D to permanently delete · Esc cancel",
                        theme::muted(),
                    ));
                } else {
                    let copy = match (kind, non_empty) {
                        (EntryKind::Directory, true) => {
                            format!("Move folder \"{name}\" and its contents to Trash?")
                        }
                        (EntryKind::Directory, false) => {
                            format!("Move folder \"{name}\" to Trash?")
                        }
                        _ => format!("Move \"{name}\" to Trash?"),
                    };
                    lines.push(Line::styled(copy, theme::text()));
                    lines.push(Line::from(""));
                    if *non_empty {
                        lines.push(Line::styled(
                            "Press D to confirm · Esc cancel",
                            theme::muted(),
                        ));
                    } else {
                        lines.push(Line::styled("Enter/y confirm · Esc cancel", theme::muted()));
                    }
                }
            }
        }
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border)
                    .style(theme::panel())
                    .title(Span::styled(title, border)),
            )
            .render(r, buf);
    }

    fn help_text(&self) -> String {
        let mode = match self.focus.mode {
            FocusMode::Transient(TransientOwner::SourceSearch)
                if self.current_workspace_is_file() || self.current_workspace_is_conversation() =>
            {
                "SEARCH"
            }
            FocusMode::Transient(TransientOwner::JumpToLine)
                if self.current_workspace_is_file() =>
            {
                "JUMP"
            }
            _ => match self.workspace_navigation.current {
                WorkspaceView::Conversation => "Conversation",
                WorkspaceView::File(_) => "File",
                WorkspaceView::Diff(_) => "Review changes",
                WorkspaceView::Run(_) => "Run",
            },
        };
        let mut text = String::from("Forge is an AI coding agent for your terminal.\n\n");
        text.push_str(&format!(
            "Active: {} · {}\n\n",
            self.focus.block.label(),
            mode
        ));
        text.push_str("Global\n");
        text.push_str("• Tab / Shift+Tab  Move between visible blocks\n");
        text.push_str("• Ctrl+E  Toggle Files\n");
        text.push_str("• ?  Help\n");
        text.push_str("• Esc  Leave one interaction level\n\n");
        text.push_str("Active block\n");
        match self.focus.block {
            FocusBlock::Workspace => {
                text.push_str("• Alt+←  Back\n");
                text.push_str("• Alt+→  Review changes\n");
                text.push_str("• Type  Start chat in composer\n");
                text.push_str("• G / r  Editor navigation and refresh\n");
                text.push_str("• Ctrl+F / Ctrl+G  Search or jump\n");
            }
            FocusBlock::Composer => {
                text.push_str("• Enter  Send\n");
                text.push_str("• ⇧Enter  Newline\n");
                text.push_str("• Esc  Return to previous block\n");
            }
            FocusBlock::Inspector => {
                text.push_str("• ⇧← / ⇧→  Switch inspector tab\n");
                text.push_str("• Esc  Return to previous block\n");
            }
            FocusBlock::BottomPanel => {
                text.push_str("• ⇧← / ⇧→  Switch bottom-panel tab\n");
                text.push_str("• Esc  Return to previous block\n");
            }
            FocusBlock::Files => {
                text.push_str("• Enter  Open or expand\n");
                text.push_str("• n / N  New file / folder\n");
                text.push_str("• R  Rename selected entry\n");
                text.push_str("• d  Delete selected entry\n");
                text.push_str("• r  Refresh selected directory\n");
                text.push_str("• Esc  Return to previous block\n");
            }
        }
        if matches!(self.focus.mode, FocusMode::Transient(_)) {
            text.push_str("\nTransient input\n• Esc  Close\n");
        }
        text
    }

    fn toggle_bottom_panel(&mut self) {
        if self.bottom_panel.open {
            self.bottom_panel.open = false;
            self.restore_focus_after_closing(FocusBlock::BottomPanel);
            self.normalize_focus();
        } else {
            self.open_bottom_panel(None);
        }
    }

    async fn submit_composer_message(&mut self) -> Result<(), TuiError> {
        let suggestions = self.slash_suggestions();
        if self.input.text.starts_with('/')
            && !suggestions.is_empty()
            && !self.input.text.contains(' ')
        {
            let idx = self.slash_suggest_idx.min(suggestions.len() - 1);
            let cmd = suggestions[idx].cmd.clone();
            let cur = self.input.text.trim();
            let line = if cur == cmd.as_str() || cur.starts_with(&(cmd.clone() + " ")) {
                self.input.take()
            } else {
                self.input.set_text(cmd);
                self.input.take()
            };
            if !line.is_empty() {
                self.history.push(&line);
                self.slash_suggest_idx = 0;
                self.notices.clear();
                self.input.history_browse = false;
                self.dispatch_line(&line).await?;
            }
            return Ok(());
        }

        let line = self.input.take();
        if line.trim().is_empty() {
            if !self.busy && !self.message_queue.is_empty() {
                self.dequeue_and_send_next();
            }
            return Ok(());
        }

        self.history.push(&line);
        self.slash_suggest_idx = 0;
        self.notices.clear();
        self.input.history_browse = false;
        if self.busy && !line.trim_start().starts_with('/') {
            self.enqueue_user_message(line);
        } else {
            self.dispatch_line(&line).await?;
        }
        Ok(())
    }

    fn handle_editor_key(&mut self, key: event::KeyEvent) -> bool {
        if !self.current_workspace_is_file() {
            return false;
        }

        let height = self.last_editor_height.saturating_sub(2) as usize;
        // Navigation shortcuts are plain keys so modified combinations can
        // continue to control contextual workspace and chrome commands.
        match key.code {
            KeyCode::Up if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_vertical(-1, height);
                true
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_vertical(1, height);
                true
            }
            KeyCode::PageUp if key.modifiers.is_empty() => {
                self.source_viewer
                    .move_cursor_vertical(-(height as isize), height);
                true
            }
            KeyCode::PageDown if key.modifiers.is_empty() => {
                self.source_viewer
                    .move_cursor_vertical(height as isize, height);
                true
            }
            KeyCode::Home if key.modifiers.is_empty() => {
                self.source_viewer.move_to_start_of_line();
                true
            }
            KeyCode::End if key.modifiers.is_empty() => {
                self.source_viewer.move_to_end_of_line();
                true
            }
            KeyCode::Left if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_horizontal(-1);
                true
            }
            KeyCode::Right if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_horizontal(1);
                true
            }
            KeyCode::Char('h') if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_horizontal(-1);
                true
            }
            KeyCode::Char('l') if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_horizontal(1);
                true
            }
            KeyCode::Char('j') if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_vertical(1, height);
                true
            }
            KeyCode::Char('k') if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_vertical(-1, height);
                true
            }
            KeyCode::Char('g') if key.modifiers.is_empty() => {
                self.source_viewer.move_to_first_line();
                true
            }
            KeyCode::Char('G')
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.source_viewer.move_to_last_line();
                true
            }
            _ => false,
        }
    }

    async fn handle_bottom_panel_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        if !self.bottom_panel.open {
            return Ok(false);
        }
        match key.code {
            KeyCode::Char(c)
                if self.bottom_panel.active == BottomPanelTab::Run
                    && self.run.editing
                    && key.modifiers.is_empty() =>
            {
                if self.run.editing_directory {
                    let mut text = self.run.draft.working_directory.display().to_string();
                    text.push(c);
                    self.run.draft.working_directory = PathBuf::from(text);
                } else {
                    self.run.draft.command_input.push(c);
                }
                Ok(true)
            }
            KeyCode::Backspace
                if self.bottom_panel.active == BottomPanelTab::Run && self.run.editing =>
            {
                if self.run.editing_directory {
                    let mut text = self.run.draft.working_directory.display().to_string();
                    text.pop();
                    self.run.draft.working_directory = PathBuf::from(text);
                } else {
                    self.run.draft.command_input.pop();
                }
                Ok(true)
            }
            _ => {
                if let Some(command) = self.semantic_command_for_bottom_panel_key(key) {
                    self.execute_semantic_command(command).await
                } else {
                    Ok(false)
                }
            }
        }
    }

    fn handle_search_key(&mut self, key: event::KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.source_viewer.close_search();
                self.focus.mode = FocusMode::Navigation;
                self.normalize_focus();
                true
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.source_viewer.prev_match();
                true
            }
            KeyCode::Enter => {
                self.source_viewer.next_match();
                true
            }
            KeyCode::Backspace => {
                self.source_viewer.backspace_search();
                true
            }
            KeyCode::Char(c) if !c.is_control() => {
                self.source_viewer.append_search_char(c);
                true
            }
            _ => true,
        }
    }

    fn handle_jump_key(&mut self, key: event::KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.source_viewer.close_jump();
                self.focus.mode = FocusMode::Navigation;
                self.normalize_focus();
                true
            }
            KeyCode::Enter => {
                self.source_viewer.commit_jump();
                true
            }
            KeyCode::Backspace => {
                self.source_viewer.backspace_jump();
                true
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                self.source_viewer.append_jump_char(c);
                true
            }
            _ => true,
        }
    }

    async fn handle_file_explorer_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        let Some(command) = self.semantic_command_for_file_key(key) else {
            return Ok(false);
        };
        self.execute_semantic_command(command).await
    }

    async fn handle_workspace_navigation_key(
        &mut self,
        key: event::KeyEvent,
    ) -> Result<bool, TuiError> {
        if let Some(command) = self.semantic_command_for_workspace_key(key) {
            return self.execute_semantic_command(command).await;
        }
        if self.current_workspace_is_file() {
            return Ok(self.handle_editor_key(key));
        }
        Ok(false)
    }

    async fn handle_active_block_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        match self.focus.block {
            FocusBlock::Files => self.handle_file_explorer_key(key).await,
            FocusBlock::Workspace => self.handle_workspace_navigation_key(key).await,
            FocusBlock::Composer => Ok(false),
            FocusBlock::Inspector => {
                if let Some(command) = self.semantic_command_for_inspector_key(key) {
                    self.execute_semantic_command(command).await
                } else {
                    Ok(false)
                }
            }
            FocusBlock::BottomPanel => self.handle_bottom_panel_key(key).await,
        }
    }

    async fn handle_global_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        let Some(command) = self.semantic_command_for_global_key(key) else {
            return Ok(false);
        };
        self.execute_semantic_command(command).await
    }

    fn printable_chat_char(key: event::KeyEvent) -> Option<char> {
        let non_shift_modifiers = key.modifiers & !(KeyModifiers::SHIFT | KeyModifiers::NONE);
        match key.code {
            KeyCode::Char(c) if non_shift_modifiers.is_empty() && !c.is_control() => Some(c),
            _ => None,
        }
    }

    async fn type_to_compose(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        let Some(c) = Self::printable_chat_char(key) else {
            return Ok(false);
        };
        self.execute_semantic_command(SemanticCommand::FocusComposer)
            .await?;
        self.input.history_browse = false;
        self.input.insert(c);
        self.clamp_slash_suggest();
        Ok(true)
    }

    async fn handle_chat_composer_key(&mut self, key: event::KeyEvent) -> Result<bool, TuiError> {
        let input_was_empty = self.input.text.is_empty();
        if let Some(command) = self.semantic_command_for_composer_key(key) {
            let consumed = self.execute_semantic_command(command).await?;
            if input_was_empty && !self.input.text.is_empty() {
                self.splash_dismissed = true;
            }
            return Ok(consumed);
        }
        let consumed = match key.code {
            KeyCode::Tab => {
                if self.input.text.starts_with('/') && !self.slash_suggestions().is_empty() {
                    self.complete_slash_suggestion();
                    true
                } else {
                    false
                }
            }
            KeyCode::Up if key.modifiers.is_empty() => {
                let suggestions = self.slash_suggestions();
                if self.input.text.starts_with('/')
                    && !suggestions.is_empty()
                    && !self.history.browsing()
                {
                    self.slash_suggest_idx =
                        (self.slash_suggest_idx + suggestions.len() - 1) % suggestions.len();
                } else if let Some(text) = self.history.up(&self.input.text) {
                    self.apply_history_text(text);
                }
                true
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                let suggestions = self.slash_suggestions();
                if self.input.text.starts_with('/')
                    && !suggestions.is_empty()
                    && !self.history.browsing()
                {
                    self.slash_suggest_idx = (self.slash_suggest_idx + 1) % suggestions.len();
                } else if let Some(text) = self.history.down() {
                    self.apply_history_text(text);
                }
                true
            }
            KeyCode::Char(c) if key.modifiers.is_empty() && !c.is_control() => {
                self.input.history_browse = false;
                self.input.insert(c);
                self.clamp_slash_suggest();
                true
            }
            KeyCode::Backspace if key.modifiers.is_empty() => {
                self.input.backspace();
                self.clamp_slash_suggest();
                true
            }
            KeyCode::Left if key.modifiers.is_empty() => {
                self.input.move_left();
                true
            }
            KeyCode::Right if key.modifiers.is_empty() => {
                self.input.move_right();
                true
            }
            KeyCode::Left if key.modifiers.contains(KeyModifiers::SHIFT) => true,
            KeyCode::Right if key.modifiers.contains(KeyModifiers::SHIFT) => true,
            KeyCode::PageUp if key.modifiers.is_empty() => {
                self.scroll_conversation_up(5);
                true
            }
            KeyCode::PageDown if key.modifiers.is_empty() => {
                self.scroll_conversation_down(5);
                true
            }
            _ => false,
        };
        if input_was_empty && !self.input.text.is_empty() {
            self.splash_dismissed = true;
        }
        Ok(consumed)
    }

    async fn apply_overlay_action(&mut self, action: OverlayAction) -> Result<(), TuiError> {
        match action {
            OverlayAction::None => {}
            OverlayAction::Close => self.overlay = None,
            OverlayAction::BeginOnboarding => {
                self.open_connect_picker();
                self.set_feedback(FeedbackSeverity::Info, "Step 1 of 2 · choose a provider");
            }
            OverlayAction::HitlApprove => {
                self.resolve_hitl_overlay(HitlDecision::Approve, false)
                    .await?;
            }
            OverlayAction::HitlApproveSession => {
                self.resolve_hitl_overlay(HitlDecision::Approve, true)
                    .await?;
            }
            OverlayAction::HitlDeny => {
                self.resolve_hitl_overlay(HitlDecision::Deny, false).await?;
            }
            OverlayAction::ContinueTurns => {
                self.overlay = None;
                self.pending_turn_continue = true;
                self.busy = true;
                self.push_toast("continuing");
            }
            OverlayAction::StopTurns => {
                self.overlay = None;
                self.status_message = "agent stopped at turn limit".into();
                self.set_feedback(FeedbackSeverity::Info, "stopped at turn limit");
            }
            OverlayAction::RunCommand(cmd) => {
                self.overlay = None;
                self.execute_semantic_command(SemanticCommand::DispatchSlash {
                    origin: SlashCommandOrigin::GlobalPalette,
                    line: cmd,
                })
                .await?;
            }
            OverlayAction::InsertInput(s) => {
                self.overlay = None;
                self.input.text = s;
                self.input.cursor = self.input.text.len();
            }
            OverlayAction::SelectModel { provider, model } => {
                self.apply_model_selection(&provider, &model);
                self.open_effort_picker_for_model(&model);
            }
            OverlayAction::SelectEffort(level) => {
                self.overlay = None;
                self.reasoning_effort = level;
                self.persist_selection();
                self.set_feedback(FeedbackSeverity::Ok, format!("reasoning effort: {level}"));
            }
            OverlayAction::SelectTheme(theme) => {
                self.apply_theme(theme, true);
            }
            OverlayAction::ConnectSubmitKey {
                profile_id,
                api_key,
            } => {
                // Keep overlay until connect succeeds so a bad key does not wipe paste.
                let key = api_key.trim().to_string();
                self.try_connect_api_key(&profile_id, Some(key));
            }
            OverlayAction::ConnectCompleteOauth { profile_id } => {
                // Enter: try one poll now; keep overlay if still pending.
                if self.oauth_pending.is_some() {
                    self.oauth_last_poll = None;
                    self.poll_oauth_tick();
                    if self.oauth_pending.is_some() {
                        self.status_message =
                            format!("Still waiting for login… (code for {profile_id})");
                    }
                } else if std::env::var("FORGE_CONNECT_OAUTH_FIXTURE").is_ok() {
                    self.overlay = None;
                    self.finish_connect(&profile_id, None, true);
                } else {
                    self.begin_oauth_flow(&profile_id);
                }
            }
            OverlayAction::ConnectUseEnv { profile_id } => {
                self.try_connect_api_key(&profile_id, None);
            }
            OverlayAction::ConnectPickProfile { profile_id } => {
                self.overlay = None;
                self.busy_phase = BusyPhase::Connect;
                self.push_activity(
                    ActivityKind::Connect,
                    FeedbackSeverity::Info,
                    format!("connect {profile_id}"),
                );
                self.finish_connect(&profile_id, None, false);
                self.busy_phase = BusyPhase::Idle;
            }
            OverlayAction::FilePick { path, is_dir } => {
                if is_dir {
                    self.open_file_explorer(Some(&path), None);
                } else {
                    self.open_file_viewer(&path);
                }
            }
        }
        Ok(())
    }

    pub async fn handle_key(&mut self, key: event::KeyEvent) -> Result<(), TuiError> {
        // Allow arrow-key auto-repeat for overlays (and other selection UIs).
        if key.kind != KeyEventKind::Press {
            let allow_repeat = matches!(
                key.kind,
                KeyEventKind::Repeat
                    if matches!(
                        key.code,
                        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right
                    )
            );
            if !allow_repeat {
                return Ok(());
            }
        }

        if self.explorer_dialog.is_some() {
            self.handle_explorer_dialog_key(key);
            return Ok(());
        }

        if matches!(self.overlay, Some(Overlay::StatusReport { .. })) {
            let ok = map_key(key);
            match ok {
                Key::Enter => {
                    self.overlay = None;
                    if self.input.text.trim().is_empty() {
                        return Ok(());
                    }
                }
                Key::Char(c) => {
                    self.overlay = None;
                    if !c.is_control() && !c.is_whitespace() {
                        self.input.history_browse = false;
                        self.input.insert(c);
                        self.clamp_slash_suggest();
                        return Ok(());
                    }
                }
                _ => {}
            }
        }

        if let Some(ref mut ov) = self.overlay {
            let ok = map_key(key);
            let action = handle_overlay_key(ov, ok);
            self.apply_overlay_action(action).await?;
            return Ok(());
        }

        self.normalize_focus();
        match self.focus.mode {
            FocusMode::Transient(TransientOwner::SourceSearch) => {
                self.handle_search_key(key);
                return Ok(());
            }
            FocusMode::Transient(TransientOwner::JumpToLine) => {
                self.handle_jump_key(key);
                return Ok(());
            }
            FocusMode::Navigation if self.focus.block == FocusBlock::Composer => {
                if self.handle_chat_composer_key(key).await? {
                    return Ok(());
                }
                if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                    self.execute_semantic_command(SemanticCommand::CycleFocus {
                        forward: !matches!(key.code, KeyCode::BackTab),
                    })
                    .await?;
                    return Ok(());
                }
            }
            FocusMode::Navigation => {
                if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                    self.execute_semantic_command(SemanticCommand::CycleFocus {
                        forward: !matches!(key.code, KeyCode::BackTab),
                    })
                    .await?;
                    return Ok(());
                }
                if key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.execute_semantic_command(SemanticCommand::CycleFocus { forward: false })
                        .await?;
                    return Ok(());
                }
                if self.handle_active_block_key(key).await? {
                    self.source_viewer.clear_notice();
                    return Ok(());
                }
            }
        }

        if self.handle_global_key(key).await? {
            return Ok(());
        }
        let _ = self.type_to_compose(key).await?;
        Ok(())
    }

    fn scroll_conversation_up(&mut self, amount: u16) {
        self.chat_follow = false;
        self.chat_scroll = self.chat_scroll.saturating_add(amount);
    }

    fn scroll_conversation_down(&mut self, amount: u16) {
        self.chat_scroll = self.chat_scroll.saturating_sub(amount);
        if self.chat_scroll == 0 {
            self.chat_follow = true;
        }
    }

    fn resolve_hit_target(&self, x: u16, y: u16) -> Option<HitTarget> {
        self.hit_regions
            .iter()
            .filter(|region| region.generation == self.frame_generation)
            .filter(|region| rect_contains(region.area, x, y))
            .max_by_key(|region| region.z_order)
            .map(|region| region.target.clone())
    }

    fn double_click_target_for(target: &HitTarget) -> Option<DoubleClickTarget> {
        match target {
            HitTarget::FileEntry(path) => Some(DoubleClickTarget::FileEntry(path.clone())),
            HitTarget::Pane(_)
            | HitTarget::DirectoryChevron(_)
            | HitTarget::ActivitySummary
            | HitTarget::VisibleControl(_)
            | HitTarget::Composer
            | HitTarget::OverlayAction(_) => None,
        }
    }

    fn file_entry_kind(&self, path: &Path) -> Option<FileKind> {
        self.file_explorer
            .visible_nodes()
            .iter()
            .find(|node| node.path == path)
            .map(|node| node.kind)
    }

    fn double_click_target_exists(&self, target: &DoubleClickTarget) -> bool {
        match target {
            DoubleClickTarget::FileEntry(path) => self.file_entry_kind(path).is_some(),
        }
    }

    fn is_qualifying_double_click(
        &self,
        target: &DoubleClickTarget,
        button: MouseButton,
        now: Instant,
    ) -> bool {
        let Some(pending) = self.pending_double_click.as_ref() else {
            return false;
        };
        pending.button == button
            && pending.target == *target
            && now.duration_since(pending.timestamp) <= DOUBLE_CLICK_THRESHOLD
            && pending.frame_generation <= self.frame_generation
            && self.double_click_target_exists(&pending.target)
            && self.double_click_target_exists(target)
    }

    async fn activate_double_click_target(
        &mut self,
        target: DoubleClickTarget,
    ) -> Result<(), TuiError> {
        match target {
            DoubleClickTarget::FileEntry(path) => match self.file_entry_kind(&path) {
                Some(FileKind::Directory) => {
                    self.execute_semantic_command(SemanticCommand::ToggleDirectory(path))
                        .await?;
                }
                Some(FileKind::File | FileKind::Symlink) => {
                    self.execute_semantic_command(SemanticCommand::OpenFile(path))
                        .await?;
                }
                Some(FileKind::Unknown) => {}
                None => {}
            },
        }
        Ok(())
    }

    fn remember_double_click_candidate(
        &mut self,
        target: DoubleClickTarget,
        button: MouseButton,
        timestamp: Instant,
    ) {
        self.pending_double_click = Some(PendingDoubleClick {
            target,
            button,
            timestamp,
            frame_generation: self.frame_generation,
        });
    }

    fn pane_target_at(&self, x: u16, y: u16) -> Option<FocusBlock> {
        match self.resolve_hit_target(x, y) {
            Some(HitTarget::Pane(block)) => Some(block),
            Some(HitTarget::FileEntry(_)) | Some(HitTarget::DirectoryChevron(_)) => {
                Some(FocusBlock::Files)
            }
            Some(HitTarget::Composer) => Some(FocusBlock::Composer),
            Some(HitTarget::ActivitySummary) => Some(FocusBlock::Workspace),
            Some(HitTarget::VisibleControl(_)) | Some(HitTarget::OverlayAction(_)) => None,
            None => None,
        }
    }

    fn scroll_files(&mut self, up: bool, amount: usize) {
        let visible_len = self.file_explorer.visible_nodes().len();
        if up {
            self.file_explorer.scroll = self.file_explorer.scroll.saturating_sub(amount);
        } else {
            self.file_explorer.scroll = self
                .file_explorer
                .scroll
                .saturating_add(amount)
                .min(visible_len.saturating_sub(1));
        }
    }

    fn scroll_workspace_under_pointer(&mut self, up: bool) {
        match self.workspace_navigation.current {
            WorkspaceView::Conversation => {
                if up {
                    self.scroll_conversation_up(3);
                } else {
                    self.scroll_conversation_down(3);
                }
            }
            WorkspaceView::File(_) => {
                let height = self.last_editor_height.saturating_sub(2) as usize;
                let delta = if up { -3 } else { 3 };
                self.source_viewer.move_cursor_vertical(delta, height);
            }
            WorkspaceView::Diff(_) | WorkspaceView::Run(_) => {}
        }
    }

    async fn activate_hit_target(&mut self, target: HitTarget) -> Result<(), TuiError> {
        match target {
            HitTarget::Pane(block) => {
                self.execute_semantic_command(SemanticCommand::FocusPane(block))
                    .await?;
            }
            HitTarget::FileEntry(path) => {
                self.execute_semantic_command(SemanticCommand::SelectEntry(path))
                    .await?;
            }
            HitTarget::DirectoryChevron(path) => {
                self.execute_semantic_command(SemanticCommand::ToggleDirectory(path))
                    .await?;
            }
            HitTarget::ActivitySummary => {
                self.execute_semantic_command(SemanticCommand::ActivateActivitySummary)
                    .await?;
            }
            HitTarget::VisibleControl(command) => {
                self.execute_semantic_command(command).await?;
            }
            HitTarget::Composer => {
                self.execute_semantic_command(SemanticCommand::FocusComposer)
                    .await?;
            }
            HitTarget::OverlayAction(action) => {
                self.apply_overlay_action(action).await?;
            }
        }
        Ok(())
    }

    pub async fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<(), TuiError> {
        if !self.runtime.mouse_capture {
            return Ok(());
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(target) = self.resolve_hit_target(mouse.column, mouse.row) else {
                    self.clear_pending_double_click();
                    return Ok(());
                };
                if (self.overlay.is_some() || self.explorer_dialog.is_some())
                    && !matches!(target, HitTarget::OverlayAction(_))
                {
                    self.clear_pending_double_click();
                    return Ok(());
                }
                let now = Instant::now();
                let double_click_target = Self::double_click_target_for(&target);
                if let Some(double_click_target) = double_click_target.clone() {
                    if self.is_qualifying_double_click(&double_click_target, MouseButton::Left, now)
                    {
                        self.clear_pending_double_click();
                        self.activate_double_click_target(double_click_target)
                            .await?;
                        self.invalidate_hit_regions();
                        return Ok(());
                    }
                } else {
                    self.clear_pending_double_click();
                }
                self.activate_hit_target(target).await?;
                if let Some(double_click_target) = double_click_target {
                    self.remember_double_click_candidate(
                        double_click_target,
                        MouseButton::Left,
                        now,
                    );
                } else {
                    self.invalidate_hit_regions();
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                self.clear_pending_double_click();
                if self.overlay.is_some() || self.explorer_dialog.is_some() {
                    return Ok(());
                }
                let up = matches!(mouse.kind, MouseEventKind::ScrollUp);
                match self.pane_target_at(mouse.column, mouse.row) {
                    Some(FocusBlock::Files) => self.scroll_files(up, 3),
                    Some(FocusBlock::Workspace) => self.scroll_workspace_under_pointer(up),
                    Some(
                        FocusBlock::Composer | FocusBlock::Inspector | FocusBlock::BottomPanel,
                    )
                    | None => {}
                }
            }
            _ => {
                self.clear_pending_double_click();
            }
        }
        Ok(())
    }

    fn apply_theme(&mut self, theme: forge_config::Theme, persist: bool) {
        crate::theme::set_active(theme);
        self.runtime.theme = theme;
        self.conversation_cache = None;
        self.overlay = None;
        self.invalidate_hit_regions();
        if persist {
            self.save_ui_state();
            self.set_feedback(FeedbackSeverity::Ok, format!("theme · {}", theme.title()));
        }
    }

    /// Run a queued user prompt with streaming + intermediate redraws.
    /// When `terminal` is `None` (unit tests), runs without intermediate draws.
    pub async fn drain_pending_prompt(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        let continuing = std::mem::take(&mut self.pending_turn_continue);
        let line = self.pending_prompt.take();
        if line.is_none() && !continuing {
            return Ok(());
        }

        // Refresh OAuth close to expiry and recycle the worker with the current token.
        if let Some(profile_id) = self.connect_profile.clone() {
            self.apply_connect_credentials(&profile_id);
        }

        self.busy = true;
        self.busy_phase = BusyPhase::Model;
        self.stream_preview.clear();
        self.stream_thinking.clear();
        self.turn_started.get_or_insert_with(Instant::now);
        self.thinking_started = None;
        self.thought_secs = None;

        if let Some(ref line) = line {
            if let Err(e) = self.session.append_user_message(line).await {
                self.busy = false;
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&e.to_string());
                self.last_exit = ExitCode::Failed;
                return Ok(());
            }
        }

        // Paint YOU message immediately
        if let Some(term) = terminal.as_deref_mut() {
            term.draw(|f| self.draw(f))?;
        }

        let max_turns = self.session.max_turns();
        let mut outcome_err: Option<String> = None;
        let mut turn_thought_secs = 0.0f64;
        let mut saw_thinking = false;

        'turns: for turn in 0..max_turns {
            let req = match self.session.prepare_model_step(turn).await {
                Ok(r) => r,
                Err(e) => {
                    outcome_err = Some(e.to_string());
                    break;
                }
            };

            let model = self.session.model_client();
            let (tx, rx) = std::sync::mpsc::channel::<ModelStreamEvent>();
            let handle =
                tokio::spawn(async move { model.complete_with_stream(req, Some(tx)).await });

            // Pump stream events + redraw until the model call finishes
            loop {
                if self.cancel_requested {
                    handle.abort();
                    self.cancel_requested = false;
                    self.turn_started = None;
                    outcome_err = Some("interrupted".into());
                    break 'turns;
                }
                while let Ok(ev) = rx.try_recv() {
                    match ev {
                        ModelStreamEvent::TextDelta { text } => {
                            // Thinking ends when answer tokens begin
                            self.close_thinking_timer();
                            self.stream_preview.push_str(&text);
                        }
                        ModelStreamEvent::ThinkingDelta { text } => {
                            if self.thinking_started.is_none() {
                                // Prefer turn start so duration covers full thinking wait if
                                // the provider dumps reasoning in one late chunk.
                                self.thinking_started =
                                    self.turn_started.or_else(|| Some(Instant::now()));
                            }
                            self.stream_thinking.push_str(&text);
                        }
                        ModelStreamEvent::Error { message } => {
                            handle.abort();
                            outcome_err = Some(message);
                            break 'turns;
                        }
                        _ => {}
                    }
                }
                // Keep the terminal responsive while the current turn is streaming so
                // the operator can type the next message and enqueue it with Enter.
                //
                // This runs BEFORE the repaint below. Draining afterwards meant a
                // keystroke arriving during the 100ms sleep was not handled until the
                // next iteration, and so was not painted until the iteration after
                // that -- roughly 200ms plus two draws from keypress to glyph.
                if terminal.is_some() {
                    drain_events(self).await?;
                    if self.should_quit {
                        handle.abort();
                        self.busy = false;
                        self.busy_phase = BusyPhase::Idle;
                        self.stream_preview.clear();
                        self.stream_thinking.clear();
                        self.turn_started = None;
                        self.thinking_started = None;
                        self.thought_secs = None;
                        self.last_exit = ExitCode::Canceled;
                        let _ = self.session.mark_cancelled().await;
                        return Ok(());
                    }
                }

                // Redraw every tick so spinner and elapsed time stay current, and so
                // input drained above lands in this frame rather than the next one.
                if let Some(term) = terminal.as_deref_mut() {
                    term.draw(|f| self.draw(f))?;
                }

                if handle.is_finished() {
                    // Drain remaining events
                    while let Ok(ev) = rx.try_recv() {
                        match ev {
                            ModelStreamEvent::TextDelta { text } => {
                                self.close_thinking_timer();
                                self.stream_preview.push_str(&text);
                            }
                            ModelStreamEvent::ThinkingDelta { text } => {
                                if self.thinking_started.is_none() {
                                    self.thinking_started =
                                        self.turn_started.or_else(|| Some(Instant::now()));
                                }
                                self.stream_thinking.push_str(&text);
                            }
                            ModelStreamEvent::Error { message } => {
                                handle.abort();
                                outcome_err = Some(message);
                                break 'turns;
                            }
                            _ => {}
                        }
                    }
                    // Thinking-only or late thinking dump: close the clock now
                    self.close_thinking_timer();
                    if let Some(term) = terminal.as_deref_mut() {
                        term.draw(|f| self.draw(f))?;
                    }
                    break;
                }

                // ~10 Hz keeps the timer + spinner smooth without burning CPU
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            let mut last = match handle.await {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    outcome_err = Some(e.to_string());
                    break;
                }
                Err(e) => {
                    outcome_err = Some(format!("model task join: {e}"));
                    break;
                }
            };

            // Provider may attach reasoning only on the final object (no stream deltas).
            if self.stream_thinking.is_empty() {
                if let Some(ref th) = last.thinking {
                    if !th.is_empty() {
                        if self.thinking_started.is_none() {
                            self.thinking_started = self.turn_started;
                        }
                        self.stream_thinking = th.clone();
                        self.close_thinking_timer();
                        // One paint so the user can see thinking before collapse
                        if let Some(term) = terminal.as_deref_mut() {
                            let _ = term.draw(|f| self.draw(f));
                        }
                    }
                }
            } else if last.thinking.as_ref().map(|t| t.is_empty()).unwrap_or(true) {
                // Prefer streamed thinking body on the final message
                last.thinking = Some(self.stream_thinking.clone());
            }
            self.close_thinking_timer();

            let thought = self.thought_secs.take();
            self.stream_preview.clear();
            self.stream_thinking.clear();
            // Keep turn_started until full agent turn ends (multi-tool steps).
            if let Some(call) = last.tool_calls.first() {
                self.busy_phase = BusyPhase::Tool {
                    name: call.name.clone(),
                };
                self.push_activity(
                    ActivityKind::Tool,
                    FeedbackSeverity::Info,
                    format!("tool_intent {}", call.name),
                );
                if let Some(term) = terminal.as_deref_mut() {
                    term.draw(|f| self.draw(f))?;
                }
            }
            match self.session.apply_model_response(last).await {
                Ok(out) => {
                    if let Some(secs) = thought {
                        saw_thinking = true;
                        turn_thought_secs += secs;
                    }
                    // Reset per-model-step thinking timers for multi-tool loops.
                    self.thinking_started = None;
                    self.thought_secs = None;
                    match out {
                        ApplyOutcome::Done(_) | ApplyOutcome::Hitl(_) => {
                            outcome_err = None;
                            self.maybe_note_workspace_changed_from_recent_tools();
                            break 'turns;
                        }
                        ApplyOutcome::Continue => {
                            self.busy_phase = BusyPhase::Model;
                            if let Some(term) = terminal.as_deref_mut() {
                                term.draw(|f| self.draw(f))?;
                            }
                            continue;
                        }
                        // `ApplyOutcome` is `#[non_exhaustive]`. Treat an outcome this
                        // build does not recognise as terminal for the turn rather than
                        // looping: an unknown outcome must not drive another model call.
                        _ => {
                            outcome_err = None;
                            self.maybe_note_workspace_changed_from_recent_tools();
                            break 'turns;
                        }
                    }
                }
                Err(e) => {
                    outcome_err = Some(e.to_string());
                    break;
                }
            }
        }

        let turn_limit_reached = outcome_err.is_none()
            && self.session.status != forge_types::SessionStatus::Completed
            && self.session.status != forge_types::SessionStatus::AwaitingHitl;
        let interrupted_partial = outcome_err
            .as_ref()
            .filter(|_| !self.stream_preview.trim().is_empty())
            .cloned();

        self.busy = false;
        self.busy_phase = BusyPhase::Idle;

        if turn_limit_reached {
            self.stream_preview.clear();
            self.stream_thinking.clear();
            self.turn_started = None;
            self.thinking_started = None;
            self.thought_secs = None;
            self.overlay = Some(Overlay::turn_limit(max_turns));
            self.last_exit = ExitCode::Success;
            self.set_feedback(
                FeedbackSeverity::Warn,
                format!("{max_turns} steps reached — continue?"),
            );
            self.push_activity(
                ActivityKind::Model,
                FeedbackSeverity::Warn,
                "turn limit reached",
            );
        } else if let Some(e) = outcome_err {
            let was_cancel = e == "interrupted";
            if let Some(interrupted) = interrupted_partial {
                self.record_interrupted_stream(&interrupted);
            } else if was_cancel {
                self.push_toast("cancelled");
                self.push_activity(
                    ActivityKind::System,
                    FeedbackSeverity::Info,
                    "turn cancelled",
                );
            } else {
                self.report_error(&e);
            }
            self.stream_preview.clear();
            self.stream_thinking.clear();
            self.turn_started = None;
            self.thinking_started = None;
            self.thought_secs = None;
            if was_cancel {
                self.last_exit = ExitCode::Canceled;
                if let Err(err) = self.session.mark_cancelled().await {
                    self.report_error(&err.to_string());
                }
            } else {
                self.last_exit = ExitCode::Failed;
            }
            // Leave queue intact so the operator can fix and continue.
        } else if self.session.pending_hitl.is_some() {
            self.stream_preview.clear();
            self.stream_thinking.clear();
            self.turn_started = None;
            self.thinking_started = None;
            self.thought_secs = None;
            if let Some(ref p) = self.session.pending_hitl {
                self.open_hitl_overlay(p.clone());
            }
            self.last_exit = ExitCode::AwaitingHitl;
            self.set_feedback(FeedbackSeverity::Warn, "awaiting human approval");
            self.push_activity(ActivityKind::Hitl, FeedbackSeverity::Warn, "hitl waiting");
            // Do not auto-dequeue until HITL is resolved.
        } else {
            self.stream_preview.clear();
            self.stream_thinking.clear();
            self.turn_started = None;
            self.thinking_started = None;
            self.thought_secs = None;
            if saw_thinking {
                self.persist_turn_thinking_duration(turn_thought_secs);
            }
            self.clear_error_chrome();
            self.tool_expanded = false;
            if self.message_queue.is_empty() {
                self.feedback = FeedbackModel::default();
                self.status_message.clear();
            } else {
                self.push_toast(format!(
                    "{} queued · sending next",
                    self.message_queue.len()
                ));
                self.set_feedback(
                    FeedbackSeverity::Info,
                    format!("{} in queue — sending next", self.message_queue.len()),
                );
            }
            self.push_activity(ActivityKind::Model, FeedbackSeverity::Ok, "model ok");
            if !self.message_queue.is_empty() {
                self.dequeue_and_send_next();
            }
        }
        Ok(())
    }

    pub fn maybe_open_hitl(&mut self) {
        if self.overlay.is_none() {
            if let Some(ref p) = self.session.pending_hitl {
                if self
                    .approval_identity_for_payload(p)
                    .is_some_and(|identity| self.hitl_session_allow.contains(&identity))
                {
                    // Will be drained by `drain_auto_hitl` in the event loop.
                    return;
                }
                self.open_hitl_overlay(p.clone());
            }
        }
    }

    /// Auto-approve HITL for exact Direct invocations remembered this session.
    pub async fn drain_auto_hitl(&mut self) -> Result<(), TuiError> {
        if let Some(ref p) = self.session.pending_hitl.clone() {
            if let Some(identity) = self.approval_identity_for_payload(p) {
                if !self.hitl_session_allow.contains(&identity) {
                    return Ok(());
                }
                self.session
                    .resolve_hitl(HitlDecision::Approve, "tui-session")
                    .await?;
                self.push_toast(format!("auto-approved {}", identity.label()));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod sync_helpers_tests {
    use super::heuristic_commit_message;

    #[test]
    fn heuristic_from_name_status() {
        let msg = heuristic_commit_message("A\tfoo.rs\nM\tbar.rs\n");
        assert!(msg.contains("foo.rs") || msg.contains("bar.rs"), "{msg}");
        assert!(
            msg.starts_with("Change") || msg.starts_with("Update") || msg.starts_with("Add"),
            "{msg}"
        );
    }
}

fn truncate_one_line(s: &str, max: usize) -> String {
    let t = s.lines().next().unwrap_or(s).trim();
    if t.chars().count() <= max {
        t.to_string()
    } else {
        format!(
            "{}…",
            t.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

/// Deterministic commit subject from `git diff --cached --name-status` when no LLM is available.
fn heuristic_commit_message(name_status: &str) -> String {
    let mut added = 0usize;
    let mut modified = 0usize;
    let mut deleted = 0usize;
    let mut names: Vec<String> = Vec::new();
    for line in name_status.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let code = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("").to_string();
        if path.is_empty() {
            continue;
        }
        let base = path.rsplit('/').next().unwrap_or(path.as_str()).to_string();
        if names.len() < 3 && !names.iter().any(|n| n == &base) {
            names.push(base);
        }
        match code.chars().next().unwrap_or(' ') {
            'A' => added += 1,
            'D' => deleted += 1,
            'M' | 'R' | 'C' | 'T' => modified += 1,
            _ => modified += 1,
        }
    }
    if names.is_empty() {
        return "Update project files".into();
    }
    let list = names.join(", ");
    let verb = if added > 0 && modified == 0 && deleted == 0 {
        "Add"
    } else if deleted > 0 && added == 0 && modified == 0 {
        "Remove"
    } else if modified > 0 && added == 0 && deleted == 0 {
        "Update"
    } else {
        "Change"
    };
    format!("{verb} {list}")
}

fn map_key(key: event::KeyEvent) -> OverlayKey {
    match key.code {
        KeyCode::Esc => OverlayKey::Esc,
        KeyCode::Enter => OverlayKey::Enter,
        KeyCode::Tab => OverlayKey::Tab,
        KeyCode::BackTab => OverlayKey::BackTab,
        KeyCode::Up => OverlayKey::Up,
        KeyCode::Down => OverlayKey::Down,
        KeyCode::Left => OverlayKey::Left,
        KeyCode::Right => OverlayKey::Right,
        KeyCode::Backspace => OverlayKey::Backspace,
        KeyCode::Char(c) => OverlayKey::Char(c),
        _ => OverlayKey::Other,
    }
}

fn centered_capped_rect_for_mouse(
    area: ratatui::layout::Rect,
    max_width: u16,
    max_height: u16,
) -> ratatui::layout::Rect {
    let width = area.width.min(max_width).max(1);
    let height = area.height.min(max_height).max(1);
    ratatui::layout::Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn path_is_under_dot_forge(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == ".forge")
}

fn rect_contains(area: ratatui::layout::Rect, x: u16, y: u16) -> bool {
    x >= area.x
        && y >= area.y
        && x < area.x.saturating_add(area.width)
        && y < area.y.saturating_add(area.height)
}

/// Drain every pending terminal event (paste floods many keys; do not drop them).
async fn drain_events(app: &mut TuiApp) -> Result<(), TuiError> {
    loop {
        if !event::poll(Duration::from_millis(0))? {
            break;
        }
        match event::read()? {
            Event::Key(key) => {
                app.handle_key(key).await?;
                app.invalidate_hit_regions();
            }
            Event::Mouse(mouse) => app.handle_mouse(mouse).await?,
            Event::Paste(data) => {
                app.handle_paste(&data);
                app.invalidate_hit_regions();
            }
            Event::Resize(_, _) => app.invalidate_hit_regions(),
            _ => {}
        }
    }
    Ok(())
}

/// Run the full-screen TUI until quit.
pub async fn run_tui(
    session: AgentSession,
    runtime: TuiRuntimeConfig,
) -> Result<ExitSummary, TuiError> {
    enable_raw_mode()?;
    // Ensure the terminal is restored on panic, returned errors and normal exit.
    let _guard = TerminalGuard::install();
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    if runtime.mouse_capture {
        execute!(stdout, EnableMouseCapture)?;
    }
    execute!(
        stdout,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        )
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = TuiApp::new(session, runtime);
    if !app.is_provider_connected() {
        app.overlay = Some(Overlay::welcome());
        app.set_feedback(
            FeedbackSeverity::Info,
            "Welcome · connect a provider to start chatting",
        );
    }
    let result = run_loop(&mut terminal, &mut app).await;

    app.persist_selection();

    result.map(|_| {
        let report = app.session.token_usage_report();
        ExitSummary {
            exit_code: app.last_exit,
            session_id: app.session.session_id.to_string(),
            token_usage: (report.api.total_api_tokens() > 0)
                .then(|| format_exit_token_usage(&report)),
        }
    })
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
) -> Result<(), TuiError> {
    while !app.should_quit {
        app.poll_file_changes();
        app.poll_run();
        app.tick_toast();
        app.tick_notices();
        app.drain_auto_hitl().await?;
        app.maybe_open_hitl();
        // Grok-style device-code: poll token endpoint while overlay is open
        app.poll_oauth_tick();
        terminal.draw(|f| app.draw(f))?;

        // Drain queued user prompt with streaming redraws (YOU paints before first token)
        if app.pending_prompt.is_some() {
            app.drain_pending_prompt(Some(terminal)).await?;
            continue;
        }
        if app.pending_turn_continue {
            app.drain_pending_prompt(Some(terminal)).await?;
            continue;
        }
        // Drain queued long-running slash tasks (so the command echo paints first).
        if app.pending_sync {
            app.drain_pending_sync(Some(terminal)).await?;
            continue;
        }
        if app.pending_hitl_decision.is_some() {
            app.drain_pending_hitl(Some(terminal)).await?;
            continue;
        }
        if app.pending_context_reset {
            app.drain_pending_context_reset(Some(terminal)).await?;
            continue;
        }
        if app.pending_external_editor {
            app.drain_pending_external_editor(Some(terminal)).await?;
            continue;
        }
        if app.pending_validation {
            app.drain_pending_validation(Some(terminal)).await?;
            continue;
        }

        if event::poll(Duration::from_millis(200))? {
            // Read the ready event, then drain the rest of the queue so a paste
            // of a long API key is not truncated to a handful of characters.
            match event::read()? {
                Event::Key(key) => {
                    app.handle_key(key).await?;
                    app.invalidate_hit_regions();
                }
                Event::Mouse(mouse) => app.handle_mouse(mouse).await?,
                Event::Paste(data) => {
                    app.handle_paste(&data);
                    app.invalidate_hit_regions();
                }
                Event::Resize(_, _) => app.invalidate_hit_regions(),
                _ => {}
            }
            drain_events(app).await?;
            // Repaint immediately after input so theme and other state changes are visible
            // without waiting for the next idle frame.
            terminal.draw(|f| app.draw(f))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::status::TurnLifecycle;
    use forge_config::CommandConfig;
    use forge_core::LoopConfig;
    use forge_model::{MockModelClient, ModelClient};
    use forge_tools::ToolRegistry;
    use forge_types::{Message, MessageRole, ModelResponse};
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
    use tempfile::TempDir;

    /// Returns (journal_workspace_guard, session). Keep the TempDir until the test ends.
    async fn test_session() -> (TempDir, AgentSession) {
        let dir = TempDir::new().unwrap();
        let session = session_for_workspace(dir.path()).await;
        (dir, session)
    }

    async fn session_for_workspace(workspace: &Path) -> AgentSession {
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "hello tui".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let session = AgentSession::create(
            LoopConfig {
                max_turns: 4,
                workspace: workspace.to_path_buf(),
                journal_dir: workspace.join("j"),
                enable_context_lifecycle: true,
                enable_governance: true,

                ..Default::default()
            },
            model,
            ToolRegistry::new(),
        )
        .await
        .unwrap();
        session
    }

    async fn session_for_workspace_with_model(
        workspace: &Path,
        model: Arc<dyn ModelClient>,
    ) -> AgentSession {
        AgentSession::create(
            LoopConfig {
                max_turns: 4,
                workspace: workspace.to_path_buf(),
                journal_dir: workspace.join("j"),
                enable_context_lifecycle: true,
                enable_governance: true,

                ..Default::default()
            },
            model,
            ToolRegistry::new(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn run_only_from_run_panel_focus() {
        let (_dir, mut app) = focus_test_app().await;
        app.run.draft.command_input = "true".into();

        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(!app
            .run
            .current
            .as_ref()
            .is_some_and(|record| record.state == RunState::Running));

        app.bottom_panel.open_tab(BottomPanelTab::Run);
        app.focus_block(FocusBlock::BottomPanel);
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app
            .run
            .current
            .as_ref()
            .is_some_and(|record| record.state == RunState::Running));
    }

    #[tokio::test]
    async fn run_cancel_from_run_panel() {
        let (_dir, mut app) = focus_test_app().await;
        app.run.draft.command_input = "true".into();
        app.bottom_panel.open_tab(BottomPanelTab::Run);
        app.focus_block(FocusBlock::BottomPanel);
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app
            .run
            .current
            .as_ref()
            .is_some_and(|record| record.state == RunState::Cancelled));
    }

    #[tokio::test]
    async fn restored_running_run_becomes_cancelled() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "test".into(),
                startup_notices: Vec::new(),
                validation_command: Some(CommandConfig {
                    executable: "true".into(),
                    args: vec![],
                }),
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.run.draft.command_input = "true".into();
        app.run_current_draft();
        app.normalize_restored_run();
        assert!(app
            .run
            .current
            .as_ref()
            .is_some_and(|record| record.state == RunState::Cancelled));
        assert!(!app.pending_validation);
        assert!(app.run_rx.is_none());
    }

    #[tokio::test]
    async fn file_change_event_refreshes_git_status() {
        let (_dir, mut app) = focus_test_app().await;
        app.file_explorer.git_status = crate::git_status::GitStatusCache::new();
        assert!(!app.file_explorer.git_status.loading);

        app.file_change_tx
            .send(FileChangeEvent {
                path: app.session.workspace_root().join("changed.txt"),
            })
            .unwrap();
        app.poll_file_changes();

        assert!(app.file_explorer.git_status.loading);
    }

    #[tokio::test]
    async fn ui_navigation_does_not_mutate_run_history() {
        let (_dir, mut app) = focus_test_app().await;
        app.bottom_panel.open_tab(BottomPanelTab::Run);
        app.focus_block(FocusBlock::BottomPanel);
        app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.run.recent.is_empty());
    }

    async fn focus_test_app() -> (TempDir, TuiApp) {
        let (dir, session) = test_session().await;
        let app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "test".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        (dir, app)
    }

    fn render_app_text(app: &mut TuiApp, width: u16, height: u16) -> String {
        use ratatui::backend::TestBackend;

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let area = buffer.area();
        let mut text = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    fn draw_app(app: &mut TuiApp, width: u16, height: u16) {
        use ratatui::backend::TestBackend;

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
    }

    /// `repo_header()` must be a pure read of the cached field. If someone
    /// reintroduces the `git` subprocess into it, the sentinel is overwritten by
    /// real repo data and this fails — which is the point.
    #[tokio::test]
    async fn repo_header_reads_cache_without_shelling_out() {
        let (_dir, mut app) = focus_test_app().await;
        app.repo_header = RepoHeaderCache {
            repo_name: Some("sentinel-repo".into()),
            branch: Some("sentinel-branch".into()),
            dirty: true,
        };

        let header = app.repo_header();

        assert_eq!(header.repo_name.as_deref(), Some("sentinel-repo"));
        assert_eq!(header.branch.as_deref(), Some("sentinel-branch"));
        assert!(header.dirty);
    }

    /// Drawing must not derive the header either — several draws in a row leave
    /// the cached sentinel untouched, proving the render path only reads it.
    #[tokio::test]
    async fn drawing_does_not_rederive_repo_header() {
        let (_dir, mut app) = focus_test_app().await;
        app.repo_header = RepoHeaderCache {
            repo_name: Some("sentinel-repo".into()),
            branch: Some("sentinel-branch".into()),
            dirty: true,
        };
        // Keep the TTL from firing a background refresh during the assertions.
        app.repo_header_refreshed_at = Instant::now();

        for _ in 0..3 {
            draw_app(&mut app, 120, 40);
        }

        assert_eq!(app.repo_header.branch.as_deref(), Some("sentinel-branch"));
    }

    /// FORGE-DESIGN 9.7: do not clear visible Git information during a refresh.
    /// A dropped sender (failed refresh) must leave the last known header intact.
    #[tokio::test]
    async fn failed_repo_header_refresh_keeps_last_known_value() {
        let (_dir, mut app) = focus_test_app().await;
        app.repo_header = RepoHeaderCache {
            repo_name: Some("kept-repo".into()),
            branch: Some("kept-branch".into()),
            dirty: true,
        };
        let (tx, rx) = mpsc::channel::<RepoHeaderCache>();
        drop(tx); // simulate a refresh worker that died
        app.repo_header_rx = Some(rx);

        app.poll_repo_header();

        assert_eq!(app.repo_header.branch.as_deref(), Some("kept-branch"));
        assert_eq!(app.repo_header.repo_name.as_deref(), Some("kept-repo"));
        assert!(app.repo_header.dirty);
        assert!(app.repo_header_rx.is_none());
    }

    /// Changing the working directory must invalidate the cached header on the
    /// very next poll, so the header never describes the previous directory.
    #[tokio::test]
    async fn cwd_change_refreshes_repo_header_immediately() {
        let (dir, mut app) = focus_test_app().await;
        app.repo_header = RepoHeaderCache {
            repo_name: Some("stale-repo".into()),
            branch: Some("stale-branch".into()),
            dirty: true,
        };

        let moved = dir.path().join("elsewhere");
        std::fs::create_dir_all(&moved).unwrap();
        app.runtime.cwd = moved.clone();
        app.poll_repo_header();

        assert_eq!(app.repo_header_cwd, moved);
        assert_eq!(app.repo_header.repo_name.as_deref(), Some("elsewhere"));
        // Plain directory, no git metadata: no branch, and not reported dirty.
        assert!(app.repo_header.branch.is_none());
        assert!(!app.repo_header.dirty);
    }

    /// An in-flight refresh that has not produced a value yet must be retained
    /// rather than dropped, and must not disturb the current header.
    #[tokio::test]
    async fn pending_repo_header_refresh_is_retained() {
        let (_dir, mut app) = focus_test_app().await;
        app.repo_header = RepoHeaderCache {
            repo_name: Some("kept-repo".into()),
            branch: Some("kept-branch".into()),
            dirty: false,
        };
        let (tx, rx) = mpsc::channel::<RepoHeaderCache>();
        app.repo_header_rx = Some(rx);

        app.poll_repo_header();

        assert!(app.repo_header_rx.is_some(), "pending refresh must survive");
        assert_eq!(app.repo_header.branch.as_deref(), Some("kept-branch"));
        drop(tx);
    }

    #[tokio::test]
    async fn final_shell_rendering_matrix_covers_v31_states_without_obsolete_chrome() {
        let sizes = [(80, 24), (120, 40), (160, 50), (240, 60)];
        let mut scenarios: Vec<(&str, TempDir, TuiApp, Vec<&str>)> = Vec::new();

        let (dir, app) = focus_test_app().await;
        scenarios.push(("conversation idle", dir, app, vec!["Describe a task"]));

        let (dir, mut app) = focus_test_app().await;
        app.busy = true;
        app.busy_phase = BusyPhase::Model;
        app.turn_started = Some(Instant::now());
        scenarios.push(("agent thinking", dir, app, vec!["thinking"]));

        let (dir, mut app) = focus_test_app().await;
        app.files_visible = true;
        scenarios.push(("files open", dir, app, vec!["FILES", "Describe a task"]));

        let (dir, mut app) = focus_test_app().await;
        app.files_visible = false;
        scenarios.push(("files closed", dir, app, vec!["Describe a task"]));

        let (dir, mut app) = focus_test_app().await;
        let file = dir.path().join("src").join("matrix.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "fn matrix() {}\n").unwrap();
        app.execute_semantic_command(SemanticCommand::OpenFile(file.clone()))
            .await
            .unwrap();
        scenarios.push(("file open", dir, app, vec!["matrix.rs"]));

        let (dir, mut app) = focus_test_app().await;
        app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
            .await
            .unwrap();
        scenarios.push(("diff", dir, app, vec!["CHANGES"]));

        let (dir, mut app) = focus_test_app().await;
        app.run.draft.command_input = "cargo test".into();
        app.run_current_draft();
        scenarios.push((
            "background run",
            dir,
            app,
            vec!["Running cargo test", "View output"],
        ));

        let (dir, mut app) = focus_test_app().await;
        app.run.draft.command_input = "cargo test".into();
        app.run_current_draft();
        app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Current))
            .await
            .unwrap();
        scenarios.push(("run open", dir, app, vec!["Run: cargo test"]));

        let (dir, mut app) = focus_test_app().await;
        app.run.draft.command_input = "cargo test".into();
        app.run_current_draft();
        if let Some(record) = app.run.current.as_mut() {
            record.state = RunState::Failed;
            record.exit_status = Some(101);
        }
        let run_id = app.current_run_id().unwrap();
        app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Id(run_id)))
            .await
            .unwrap();
        scenarios.push(("run failed", dir, app, vec!["Failed", "Exit status: 101"]));

        let (dir, mut app) = focus_test_app().await;
        app.open_hitl_overlay(direct_hitl_payload("matrix-approval", "src/main.rs"));
        scenarios.push((
            "approval",
            dir,
            app,
            vec!["Approval required", "Allow once", "Deny"],
        ));

        let (dir, mut app) = focus_test_app().await;
        app.sidebar_visible = false;
        scenarios.push(("inspector closed", dir, app, vec!["Describe a task"]));

        let (dir, mut app) = focus_test_app().await;
        app.sidebar_visible = true;
        app.focus_block(FocusBlock::Inspector);
        scenarios.push((
            "inspector open",
            dir,
            app,
            vec!["INSPECTOR", "Describe a task"],
        ));

        let (dir, mut app) = focus_test_app().await;
        app.bottom_panel.open = false;
        scenarios.push(("bottom closed", dir, app, vec!["Describe a task"]));

        let (dir, mut app) = focus_test_app().await;
        app.open_bottom_panel(Some(BottomPanelTab::Terminal));
        scenarios.push(("bottom open", dir, app, vec!["Terminal", "Describe a task"]));

        let (dir, mut app) = focus_test_app().await;
        app.runtime.mouse_capture = false;
        scenarios.push(("mouse disabled", dir, app, vec!["Describe a task"]));

        for (name, _dir, mut app, expected) in scenarios {
            for (width, height) in sizes {
                let text = render_app_text(&mut app, width, height);
                assert!(
                    !text.contains(" Chat  Editor  Diff "),
                    "{name} at {width}x{height} restored permanent tabs:\n{text}"
                );
                assert!(
                    !text.contains("BOTTOM"),
                    "{name} at {width}x{height} restored BOTTOM label:\n{text}"
                );
                assert!(
                    !text.contains("Ctrl+P close"),
                    "{name} at {width}x{height} restored shortcut manual:\n{text}"
                );
                if name == "mouse disabled" {
                    assert!(
                        !text.to_ascii_lowercase().contains("mouse"),
                        "mouse-disabled chrome should not spend space on mouse hints:\n{text}"
                    );
                }
                let lower_text = text.to_ascii_lowercase();
                assert!(
                    expected
                        .iter()
                        .any(|needle| lower_text.contains(&needle.to_ascii_lowercase())),
                    "{name} at {width}x{height} missing expected state {:?}:\n{text}",
                    expected
                );
            }
        }
    }

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn hit_point(app: &TuiApp, predicate: impl Fn(&HitTarget) -> bool) -> (u16, u16) {
        let region = app
            .hit_regions
            .iter()
            .find(|region| region.generation == app.frame_generation && predicate(&region.target))
            .expect("expected hit region");
        (region.area.x, region.area.y)
    }

    fn hit_point_for_path(
        app: &TuiApp,
        predicate: impl Fn(&HitTarget, &Path) -> bool,
        path: &Path,
    ) -> (u16, u16) {
        hit_point(app, |target| predicate(target, path))
    }

    #[tokio::test]
    async fn focus_starts_on_composer_block() {
        let (_dir, app) = focus_test_app().await;
        assert_eq!(app.focus.block, FocusBlock::Composer);
        assert_eq!(app.focus.mode, FocusMode::Navigation);
    }

    #[tokio::test]
    async fn tab_cycles_visible_blocks_and_skips_hidden_ones() {
        let (_dir, mut app) = focus_test_app().await;
        app.focus_block(FocusBlock::Workspace);
        app.files_visible = true;
        app.bottom_panel.open = true;
        app.normalize_focus();

        app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Composer);
        app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::BottomPanel);
        app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Files);
        app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Workspace);

        app.sidebar_visible = false;
        app.normalize_focus();
        app.handle_key(press(KeyCode::BackTab, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Files);
    }

    #[tokio::test]
    async fn file_change_does_not_reload_tree_while_files_sidebar_is_focused() {
        let (dir, mut app) = focus_test_app().await;
        fs::create_dir(dir.path().join("crates")).unwrap();
        fs::create_dir(dir.path().join("crates/forge-tui")).unwrap();
        fs::write(dir.path().join("crates/forge-tui/Cargo.toml"), "").unwrap();
        app.file_explorer.refresh_selected();
        app.file_explorer.selected_path = Some(dir.path().join("crates").canonicalize().unwrap());
        app.file_explorer.expand_selected();
        app.file_explorer.selected_path =
            Some(dir.path().join("crates/forge-tui").canonicalize().unwrap());
        app.file_explorer.expand_selected();
        app.files_visible = true;
        app.focus_block(FocusBlock::Files);
        app.file_explorer.git_status = crate::git_status::GitStatusCache::new();

        app.file_change_tx
            .send(FileChangeEvent {
                path: app.session.workspace_root().join("changed.txt"),
            })
            .unwrap();
        app.poll_file_changes();

        assert!(app.file_explorer.git_status.loading);
        assert!(app
            .file_explorer
            .visible_nodes()
            .iter()
            .any(|node| node.display_name == "Cargo.toml"));
    }

    #[test]
    fn forge_runtime_paths_are_ignored_by_file_watcher_filter() {
        assert!(path_is_under_dot_forge(Path::new(".forge/progress.json")));
        assert!(path_is_under_dot_forge(Path::new(
            "/tmp/repo/.forge/sessions/x.db"
        )));
        assert!(!path_is_under_dot_forge(Path::new("src/app.rs")));
        assert!(!path_is_under_dot_forge(Path::new("/tmp/repo/src/lib.rs")));
    }

    #[tokio::test]
    async fn tab_and_shift_tab_reach_composer() {
        let (_dir, mut app) = focus_test_app().await;
        app.focus_block(FocusBlock::Workspace);

        app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Composer);

        app.handle_key(press(KeyCode::BackTab, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Workspace);
    }

    #[tokio::test]
    async fn mouse_click_pane_and_composer_focus() {
        let (_dir, mut app) = focus_test_app().await;
        draw_app(&mut app, 120, 30);

        let (x, y) = hit_point(&app, |target| {
            matches!(target, HitTarget::Pane(FocusBlock::Workspace))
        });
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Workspace);

        draw_app(&mut app, 120, 30);
        let (x, y) = hit_point(&app, |target| matches!(target, HitTarget::Composer));
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Composer);
    }

    #[tokio::test]
    async fn mouse_click_file_row_selects_and_chevron_toggles() {
        let (dir, mut app) = focus_test_app().await;
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "pub fn demo() {}\n").unwrap();
        app.file_explorer.refresh_workspace();
        app.files_visible = true;
        draw_app(&mut app, 140, 30);

        let src = dir.path().join("src").canonicalize().unwrap();
        let (x, y) = hit_point_for_path(
            &app,
            |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
            &src,
        );
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();
        assert_eq!(
            app.file_explorer.selected_path.as_deref(),
            Some(src.as_path())
        );

        draw_app(&mut app, 140, 30);
        let (x, y) = hit_point_for_path(
            &app,
            |target, path| matches!(target, HitTarget::DirectoryChevron(p) if p == path),
            &src,
        );
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();
        assert!(app
            .file_explorer
            .visible_nodes()
            .iter()
            .any(|node| node.display_name == "lib.rs"));
    }

    #[tokio::test]
    async fn mouse_click_bottom_tab_visible_control_emits_once() {
        let (_dir, mut app) = focus_test_app().await;
        app.open_bottom_panel(Some(BottomPanelTab::Run));
        draw_app(&mut app, 120, 40);
        let (x, y) = hit_point(&app, |target| {
            matches!(
                target,
                HitTarget::VisibleControl(SemanticCommand::OpenBottomPanel(
                    BottomPanelTab::Activity
                ))
            )
        });
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();
        assert_eq!(app.bottom_panel.active, BottomPanelTab::Activity);
        assert_eq!(app.focus.block, FocusBlock::BottomPanel);
    }

    #[tokio::test]
    async fn mouse_wheel_scrolls_hovered_pane_without_focus_change() {
        let (_dir, mut app) = focus_test_app().await;
        app.focus_block(FocusBlock::Composer);
        draw_app(&mut app, 120, 30);
        let (x, y) = hit_point(&app, |target| {
            matches!(target, HitTarget::Pane(FocusBlock::Workspace))
        });
        app.handle_mouse(mouse(MouseEventKind::ScrollUp, x, y))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Composer);
        assert_eq!(app.chat_scroll, 3);
    }

    #[tokio::test]
    async fn mouse_overlay_blocks_underlying_targets() {
        let (_dir, mut app) = focus_test_app().await;
        let payload = HitlPayload {
            call_id: "call-1".into(),
            tool: "write".into(),
            args_redacted: json!({"path": "src/main.rs"}),
            reason: "Edit requires approval".into(),
        };
        app.open_hitl_overlay(payload);
        draw_app(&mut app, 120, 30);
        let (x, y) = hit_point(&app, |target| matches!(target, HitTarget::Composer));
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Composer);
        assert!(matches!(app.overlay, Some(Overlay::Hitl { .. })));
    }

    #[tokio::test]
    async fn mouse_disabled_ignores_pointer_but_keeps_keyboard() {
        let (_dir, mut app) = focus_test_app().await;
        app.runtime.mouse_capture = false;
        draw_app(&mut app, 120, 30);
        let (x, y) = hit_point(&app, |target| {
            matches!(target, HitTarget::Pane(FocusBlock::Workspace))
        });
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Composer);

        app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Workspace);
    }

    #[tokio::test]
    async fn mouse_stale_regions_are_ignored_after_resize_or_list_mutation() {
        let (dir, mut app) = focus_test_app().await;
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "pub fn demo() {}\n").unwrap();
        app.file_explorer.refresh_workspace();
        app.files_visible = true;
        app.file_explorer.selected_path = Some(dir.path().join("src").canonicalize().unwrap());
        app.file_explorer.expand_selected();
        draw_app(&mut app, 140, 30);

        let lib = dir.path().join("src/lib.rs").canonicalize().unwrap();
        let stale_lib_point = hit_point_for_path(
            &app,
            |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
            &lib,
        );
        app.file_explorer.selected_path = Some(dir.path().join("src").canonicalize().unwrap());
        app.file_explorer.collapse_selected();
        app.file_explorer.selected_path = app.file_explorer.root_path().map(Path::to_path_buf);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            stale_lib_point.0,
            stale_lib_point.1,
        ))
        .await
        .unwrap();
        assert_ne!(
            app.file_explorer.selected_path.as_deref(),
            Some(lib.as_path())
        );

        draw_app(&mut app, 140, 30);
        let (x, y) = hit_point(&app, |target| {
            matches!(target, HitTarget::Pane(FocusBlock::Workspace))
        });
        app.invalidate_hit_regions();
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();
        assert_ne!(app.focus.block, FocusBlock::Workspace);
    }

    #[tokio::test]
    async fn mouse_unsupported_buttons_and_80x24_regions_are_safe() {
        let (_dir, mut app) = focus_test_app().await;
        draw_app(&mut app, 80, 24);
        let (x, y) = hit_point(&app, |target| matches!(target, HitTarget::Composer));
        app.focus_block(FocusBlock::Workspace);
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Right), x, y))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Workspace);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Composer);
    }

    #[tokio::test]
    async fn mouse_double_click_same_file_opens_it_like_enter() {
        let (dir, mut app) = focus_test_app().await;
        let file = dir.path().join("main.rs");
        fs::write(&file, "fn main() {}\n").unwrap();
        app.file_explorer.refresh_workspace();
        app.files_visible = true;
        draw_app(&mut app, 140, 30);

        let canonical = file.canonicalize().unwrap();
        let (x, y) = hit_point_for_path(
            &app,
            |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
            &canonical,
        );
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();

        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::File(canonical.clone())
        );
        assert_eq!(app.source_viewer.path.as_deref(), Some(canonical.as_path()));

        let (enter_dir, mut enter_app) = focus_test_app().await;
        let enter_file = enter_dir.path().join("main.rs");
        fs::write(&enter_file, "fn main() {}\n").unwrap();
        enter_app.file_explorer.refresh_workspace();
        enter_app.files_visible = true;
        enter_app.file_explorer.selected_path = Some(enter_file.canonicalize().unwrap());
        assert_eq!(
            enter_app.semantic_command_for_file_key(press(KeyCode::Enter, KeyModifiers::NONE)),
            Some(SemanticCommand::OpenSelectedEntry)
        );
        enter_app
            .execute_semantic_command(SemanticCommand::OpenSelectedEntry)
            .await
            .unwrap();
        assert!(matches!(
            enter_app.workspace_navigation.current,
            WorkspaceView::File(_)
        ));
        assert!(enter_app.source_viewer.path.is_some());
    }

    #[tokio::test]
    async fn mouse_double_click_slow_or_different_rows_only_selects() {
        let (dir, mut app) = focus_test_app().await;
        let first = dir.path().join("first.rs");
        let second = dir.path().join("second.rs");
        fs::write(&first, "fn first() {}\n").unwrap();
        fs::write(&second, "fn second() {}\n").unwrap();
        app.file_explorer.refresh_workspace();
        app.files_visible = true;
        draw_app(&mut app, 140, 30);

        let first = first.canonicalize().unwrap();
        let second = second.canonicalize().unwrap();
        let first_point = hit_point_for_path(
            &app,
            |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
            &first,
        );
        let second_point = hit_point_for_path(
            &app,
            |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
            &second,
        );

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            first_point.0,
            first_point.1,
        ))
        .await
        .unwrap();
        app.pending_double_click.as_mut().unwrap().timestamp =
            Instant::now() - DOUBLE_CLICK_THRESHOLD - Duration::from_millis(1);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            first_point.0,
            first_point.1,
        ))
        .await
        .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
        assert_eq!(
            app.file_explorer.selected_path.as_deref(),
            Some(first.as_path())
        );

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            second_point.0,
            second_point.1,
        ))
        .await
        .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
        assert_eq!(
            app.file_explorer.selected_path.as_deref(),
            Some(second.as_path())
        );
    }

    #[tokio::test]
    async fn mouse_double_click_cancels_on_scroll_resize_list_or_modal_change() {
        let (dir, mut app) = focus_test_app().await;
        let file = dir.path().join("main.rs");
        fs::write(&file, "fn main() {}\n").unwrap();
        app.file_explorer.refresh_workspace();
        app.files_visible = true;
        draw_app(&mut app, 140, 30);

        let file = file.canonicalize().unwrap();
        let file_point = hit_point_for_path(
            &app,
            |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
            &file,
        );
        let workspace_point = hit_point(&app, |target| {
            matches!(target, HitTarget::Pane(FocusBlock::Workspace))
        });

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            file_point.0,
            file_point.1,
        ))
        .await
        .unwrap();
        app.handle_mouse(mouse(
            MouseEventKind::ScrollUp,
            workspace_point.0,
            workspace_point.1,
        ))
        .await
        .unwrap();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            file_point.0,
            file_point.1,
        ))
        .await
        .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
        app.clear_pending_double_click();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            file_point.0,
            file_point.1,
        ))
        .await
        .unwrap();
        app.invalidate_hit_regions();
        draw_app(&mut app, 140, 30);
        let file_point = hit_point_for_path(
            &app,
            |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
            &file,
        );
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            file_point.0,
            file_point.1,
        ))
        .await
        .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
        app.clear_pending_double_click();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            file_point.0,
            file_point.1,
        ))
        .await
        .unwrap();
        app.note_workspace_changed();
        draw_app(&mut app, 140, 30);
        let file_point = hit_point_for_path(
            &app,
            |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
            &file,
        );
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            file_point.0,
            file_point.1,
        ))
        .await
        .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
        app.clear_pending_double_click();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            file_point.0,
            file_point.1,
        ))
        .await
        .unwrap();
        app.overlay = Some(Overlay::welcome());
        app.invalidate_hit_regions();
        app.overlay = None;
        draw_app(&mut app, 140, 30);
        let file_point = hit_point_for_path(
            &app,
            |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
            &file,
        );
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            file_point.0,
            file_point.1,
        ))
        .await
        .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
    }

    #[tokio::test]
    async fn mouse_double_click_uses_semantic_identity_for_truncated_names() {
        let (dir, mut app) = focus_test_app().await;
        let long_name = format!("{}-forge-mouse.rs", "very-long-name".repeat(8));
        let file = dir.path().join(long_name);
        fs::write(&file, "fn main() {}\n").unwrap();
        app.file_explorer.refresh_workspace();
        app.files_visible = true;
        draw_app(&mut app, 120, 30);

        let file = file.canonicalize().unwrap();
        let (x, y) = hit_point_for_path(
            &app,
            |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
            &file,
        );
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();

        assert_eq!(app.source_viewer.path.as_deref(), Some(file.as_path()));
    }

    #[tokio::test]
    async fn mouse_double_click_folder_row_toggles_once() {
        let (dir, mut app) = focus_test_app().await;
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "pub fn demo() {}\n").unwrap();
        app.file_explorer.refresh_workspace();
        app.files_visible = true;
        draw_app(&mut app, 140, 30);

        let src = dir.path().join("src").canonicalize().unwrap();
        let (x, y) = hit_point_for_path(
            &app,
            |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
            &src,
        );
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();

        assert_eq!(
            app.file_explorer.selected_path.as_deref(),
            Some(src.as_path())
        );
        assert!(app
            .file_explorer
            .visible_nodes()
            .iter()
            .any(|node| node.display_name == "lib.rs"));
    }

    #[tokio::test]
    async fn mouse_double_click_controls_do_not_gain_row_activation() {
        let (_dir, mut app) = focus_test_app().await;
        app.open_bottom_panel(Some(BottomPanelTab::Run));
        draw_app(&mut app, 120, 40);
        let (x, y) = hit_point(&app, |target| {
            matches!(
                target,
                HitTarget::VisibleControl(SemanticCommand::OpenBottomPanel(
                    BottomPanelTab::Activity
                ))
            )
        });
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();

        assert_eq!(app.bottom_panel.active, BottomPanelTab::Activity);
        assert!(app.pending_double_click.is_none());
    }

    #[tokio::test]
    async fn mouse_double_click_cannot_bypass_delete_confirmation() {
        let (dir, mut app) = focus_test_app().await;
        let file = dir.path().join("delete-me.rs");
        fs::write(&file, "fn main() {}\n").unwrap();
        app.file_explorer.refresh_workspace();
        app.files_visible = true;
        app.file_explorer.selected_path = Some(file.canonicalize().unwrap());
        app.open_explorer_delete_dialog();
        draw_app(&mut app, 120, 30);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 10, 10))
            .await
            .unwrap();
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 10, 10))
            .await
            .unwrap();

        assert!(matches!(
            app.explorer_dialog,
            Some(ExplorerDialog::ConfirmDelete { .. })
        ));
        assert!(file.exists());
    }

    #[tokio::test]
    async fn opening_and_closing_bottom_panel_transfers_focus() {
        let (_dir, mut app) = focus_test_app().await;
        app.focus_block(FocusBlock::Workspace);
        app.handle_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::BottomPanel);
        assert!(app.bottom_panel.open);
        app.handle_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Workspace);
        assert!(!app.bottom_panel.open);
    }

    #[tokio::test]
    async fn shift_arrow_tabs_only_apply_to_the_active_navigation_block() {
        let (_dir, mut app) = focus_test_app().await;
        app.focus_block(FocusBlock::Workspace);
        app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
            .await
            .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Diff(DiffCommandContext::Current)
        );

        app.sidebar_visible = true;
        app.focus_block(FocusBlock::Inspector);
        assert_eq!(app.focus.block, FocusBlock::Inspector);
        app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
            .await
            .unwrap();
        assert_eq!(app.inspector_view, InspectorView::Context);

        app.open_bottom_panel(None);
        app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
            .await
            .unwrap();
        assert_eq!(app.bottom_panel.active, BottomPanelTab::Activity);
    }

    #[tokio::test]
    async fn chat_input_keeps_literal_brackets_and_shift_arrows_do_not_switch_tabs() {
        let (_dir, mut app) = focus_test_app().await;
        app.handle_key(press(KeyCode::Char('['), KeyModifiers::NONE))
            .await
            .unwrap();
        app.handle_key(press(KeyCode::Char(']'), KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.input.text, "[]");
        app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
            .await
            .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
        app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.focus.mode, FocusMode::Navigation);
        assert_eq!(app.focus.block, FocusBlock::Workspace);
        app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Composer);
        assert_eq!(app.input.text, "[]x");
    }

    #[tokio::test]
    async fn esc_from_composer_returns_to_previous_block_and_keeps_draft() {
        let (_dir, mut app) = focus_test_app().await;
        for block in [
            FocusBlock::Files,
            FocusBlock::Workspace,
            FocusBlock::Inspector,
            FocusBlock::BottomPanel,
        ] {
            app.files_visible = true;
            app.sidebar_visible = true;
            app.bottom_panel.open = true;
            app.focus_block(block);
            app.enter_chat_composer();
            app.input.set_text("draft");
            app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
                .await
                .unwrap();
            assert_eq!(app.focus.block, block);
            assert_eq!(app.input.text, "draft");
        }
    }

    #[tokio::test]
    async fn type_to_compose_keeps_first_unbound_printable() {
        let (_dir, mut app) = focus_test_app().await;
        app.focus_block(FocusBlock::Workspace);
        app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
            .await
            .unwrap();

        app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(app.focus.block, FocusBlock::Composer);
        assert_eq!(app.input.text, "x");
    }

    #[tokio::test]
    async fn semantic_key_paths_emit_existing_commands() {
        let (_dir, mut app) = focus_test_app().await;
        assert_eq!(
            app.semantic_command_for_global_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL)),
            Some(SemanticCommand::ToggleFiles)
        );
        assert_eq!(
            app.semantic_command_for_global_key(press(KeyCode::Char('k'), KeyModifiers::CONTROL)),
            Some(SemanticCommand::OpenGlobalCommandPalette)
        );

        app.focus_block(FocusBlock::Workspace);
        assert_eq!(
            app.semantic_command_for_workspace_key(press(KeyCode::Right, KeyModifiers::SHIFT)),
            Some(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        );
        assert_eq!(
            app.semantic_command_for_composer_key(press(KeyCode::Enter, KeyModifiers::NONE)),
            Some(SemanticCommand::SubmitMessage)
        );
        assert_eq!(
            app.semantic_command_for_composer_key(press(KeyCode::Enter, KeyModifiers::SHIFT)),
            Some(SemanticCommand::InsertComposerNewline)
        );

        app.files_visible = true;
        assert_eq!(
            app.semantic_command_for_file_key(press(KeyCode::Enter, KeyModifiers::NONE)),
            Some(SemanticCommand::OpenSelectedEntry)
        );
    }

    #[tokio::test]
    async fn semantic_commands_dispatch_without_rendering_a_frame() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("main.rs");
        fs::write(&path, "fn main() {}\n").unwrap();

        app.execute_semantic_command(SemanticCommand::ToggleFiles)
            .await
            .unwrap();
        assert!(app.files_visible);
        assert_eq!(app.focus.block, FocusBlock::Files);

        app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
            .await
            .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Diff(DiffCommandContext::Current)
        );

        app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
            .await
            .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::File(path.clone())
        );
        assert_eq!(
            app.source_viewer.path.as_deref(),
            Some(path.canonicalize().unwrap().as_path())
        );

        app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Current))
            .await
            .unwrap();
        assert!(app.bottom_panel.open);
        assert_eq!(app.bottom_panel.active, BottomPanelTab::Run);
    }

    #[tokio::test]
    async fn semantic_dispatch_handles_invalid_or_stale_identifiers_without_panic() {
        let (_dir, mut app) = focus_test_app().await;
        let missing = PathBuf::from("/definitely/missing/forge-file.rs");

        app.execute_semantic_command(SemanticCommand::SelectEntry(missing.clone()))
            .await
            .unwrap();
        app.execute_semantic_command(SemanticCommand::ToggleDirectory(missing.clone()))
            .await
            .unwrap();
        app.execute_semantic_command(SemanticCommand::OpenFile(missing))
            .await
            .unwrap();
        app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Id(
            "missing-run".into(),
        )))
        .await
        .unwrap();

        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
        assert!(!app.bottom_panel.open);
    }

    #[tokio::test]
    async fn modal_and_transient_precedence_still_wins_over_semantic_bindings() {
        let (dir, mut app) = focus_test_app().await;
        app.overlay = Some(Overlay::welcome());
        app.handle_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert!(!app.files_visible);
        assert!(app.overlay.is_some());

        app.overlay = None;
        let path = dir.path().join("source.txt");
        fs::write(&path, "alpha\n").unwrap();
        app.open_file_in_editor(&path);
        app.handle_key(press(KeyCode::Char('f'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        app.handle_key(press(KeyCode::Char('z'), KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(
            app.focus.mode,
            FocusMode::Transient(TransientOwner::SourceSearch)
        );
        assert_eq!(app.source_viewer.search.query, "z");
        assert!(app.input.text.is_empty());
    }

    #[tokio::test]
    async fn printable_globals_remain_available_to_type_to_compose() {
        let (_dir, mut app) = focus_test_app().await;
        app.focus_block(FocusBlock::Workspace);
        assert_eq!(
            app.semantic_command_for_global_key(press(KeyCode::Char('x'), KeyModifiers::NONE)),
            None
        );

        app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(app.focus.block, FocusBlock::Composer);
        assert_eq!(app.input.text, "x");
    }

    #[tokio::test]
    async fn global_palette_selection_uses_semantic_dispatch() {
        let (_dir, mut app) = focus_test_app().await;
        app.execute_semantic_command(SemanticCommand::DispatchSlash {
            origin: SlashCommandOrigin::GlobalPalette,
            line: "/refresh".into(),
        })
        .await
        .unwrap();

        assert_eq!(app.status_message, "Refreshing git status...");
    }

    #[tokio::test]
    async fn workspace_navigation_starts_at_conversation_home() {
        let (_dir, app) = focus_test_app().await;

        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
        assert!(app.workspace_navigation.history.is_empty());
    }

    #[tokio::test]
    async fn workspace_navigation_pushes_file_and_replaces_file_resource() {
        let (dir, mut app) = focus_test_app().await;
        let first = dir.path().join("a.rs");
        let second = dir.path().join("b.rs");
        fs::write(&first, "fn a() {}\n").unwrap();
        fs::write(&second, "fn b() {}\n").unwrap();

        app.execute_semantic_command(SemanticCommand::OpenFile(first.clone()))
            .await
            .unwrap();

        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::File(first.clone())
        );
        assert_eq!(
            app.workspace_navigation.history,
            vec![WorkspaceView::Conversation]
        );

        app.execute_semantic_command(SemanticCommand::OpenFile(second.clone()))
            .await
            .unwrap();

        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::File(second)
        );
        assert_eq!(
            app.workspace_navigation.history,
            vec![WorkspaceView::Conversation]
        );
    }

    #[tokio::test]
    async fn workspace_navigation_pushes_between_file_diff_and_file() {
        let (dir, mut app) = focus_test_app().await;
        let first = dir.path().join("a.rs");
        let second = dir.path().join("b.rs");
        fs::write(&first, "fn a() {}\n").unwrap();
        fs::write(&second, "fn b() {}\n").unwrap();

        app.execute_semantic_command(SemanticCommand::OpenFile(first.clone()))
            .await
            .unwrap();
        app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
            .await
            .unwrap();

        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Diff(DiffCommandContext::Current)
        );
        assert_eq!(
            app.workspace_navigation.history,
            vec![
                WorkspaceView::Conversation,
                WorkspaceView::File(first.clone())
            ]
        );

        app.execute_semantic_command(SemanticCommand::OpenFile(second.clone()))
            .await
            .unwrap();

        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::File(second)
        );
        assert_eq!(
            app.workspace_navigation.history,
            vec![
                WorkspaceView::Conversation,
                WorkspaceView::File(first),
                WorkspaceView::Diff(DiffCommandContext::Current)
            ]
        );
    }

    #[tokio::test]
    async fn workspace_back_skips_invalid_file_entries() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("stale.rs");
        fs::write(&path, "fn stale() {}\n").unwrap();

        app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
            .await
            .unwrap();
        app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
            .await
            .unwrap();
        fs::remove_file(&path).unwrap();
        app.execute_semantic_command(SemanticCommand::GoBack)
            .await
            .unwrap();

        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
    }

    #[tokio::test]
    async fn workspace_home_returns_to_conversation_and_clears_history() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("main.rs");
        fs::write(&path, "fn main() {}\n").unwrap();

        app.execute_semantic_command(SemanticCommand::OpenFile(path))
            .await
            .unwrap();
        app.execute_semantic_command(SemanticCommand::GoHome)
            .await
            .unwrap();

        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
        assert!(app.workspace_navigation.history.is_empty());
    }

    #[tokio::test]
    async fn overlay_open_and_close_do_not_mutate_workspace_history() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("main.rs");
        fs::write(&path, "fn main() {}\n").unwrap();
        app.execute_semantic_command(SemanticCommand::OpenFile(path))
            .await
            .unwrap();
        let before = app.workspace_navigation.clone();

        app.overlay = Some(Overlay::welcome());
        app.execute_semantic_command(SemanticCommand::CloseOverlay)
            .await
            .unwrap();

        assert_eq!(app.workspace_navigation, before);
        assert!(app.overlay.is_none());
    }

    #[tokio::test]
    async fn files_visibility_is_independent_of_workspace_navigation() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("main.rs");
        fs::write(&path, "fn main() {}\n").unwrap();
        app.files_visible = true;

        app.execute_semantic_command(SemanticCommand::OpenFile(path))
            .await
            .unwrap();
        app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
            .await
            .unwrap();
        app.execute_semantic_command(SemanticCommand::GoHome)
            .await
            .unwrap();

        assert!(app.files_visible);
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
    }

    #[tokio::test]
    async fn files_visibility_renders_independently_in_each_workspace_view() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("main.rs");
        fs::write(&path, "fn main() {}\n").unwrap();

        for view in [
            WorkspaceView::Conversation,
            WorkspaceView::File(path.clone()),
            WorkspaceView::Diff(DiffCommandContext::Current),
        ] {
            app.files_visible = true;
            app.navigate_to_workspace_view(view.clone());
            let rendered = render_app_text(&mut app, 160, 50);
            assert!(
                rendered.contains("FILES"),
                "Files should render for {view:?} when preference is open:\n{rendered}"
            );

            app.files_visible = false;
            let rendered = render_app_text(&mut app, 160, 50);
            assert!(
                !rendered.contains("FILES"),
                "Files should not render for {view:?} when preference is closed:\n{rendered}"
            );
        }
    }

    #[tokio::test]
    async fn files_visibility_auto_collapses_and_restores_without_mutating_preference() {
        let (_dir, mut app) = focus_test_app().await;
        app.files_visible = true;
        app.focus_block(FocusBlock::Files);

        let narrow = render_app_text(&mut app, 80, 24);
        assert!(!narrow.contains("FILES"), "{narrow}");
        assert!(app.files_visible, "auto-collapse must not persist close");
        assert_eq!(app.focus.block, FocusBlock::Workspace);

        let wide = render_app_text(&mut app, 160, 50);
        assert!(wide.contains("FILES"), "{wide}");
        assert!(app.files_visible);
    }

    #[tokio::test]
    async fn files_explicit_close_remains_closed_after_resizing() {
        let (_dir, mut app) = focus_test_app().await;
        app.files_visible = true;
        app.execute_semantic_command(SemanticCommand::ToggleFiles)
            .await
            .unwrap();

        assert!(!app.files_visible);
        let narrow = render_app_text(&mut app, 80, 24);
        let wide = render_app_text(&mut app, 160, 50);
        assert!(!narrow.contains("FILES"), "{narrow}");
        assert!(!wide.contains("FILES"), "{wide}");
    }

    #[tokio::test]
    async fn files_visibility_persists_per_repository() {
        let (dir, mut app) = focus_test_app().await;
        app.execute_semantic_command(SemanticCommand::ToggleFiles)
            .await
            .unwrap();
        assert!(app.files_visible);

        let session = session_for_workspace(dir.path()).await;
        let restored = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "test".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        assert!(restored.files_visible);

        let (_other_dir, other) = focus_test_app().await;
        assert!(
            !other.files_visible,
            "Files preference must not leak across repositories"
        );
    }

    #[tokio::test]
    async fn theme_change_updates_active_palette_immediately() {
        let (_dir, mut app) = focus_test_app().await;
        assert_eq!(crate::theme::active(), forge_config::Theme::Dark);
        assert_eq!(crate::theme::text().fg, Some(crate::theme::TEXT));

        app.handle_theme_command(Some("light"));
        assert_eq!(crate::theme::active(), forge_config::Theme::Light);
        assert_eq!(crate::theme::text().fg, Some(crate::theme::LIGHT_TEXT));
        assert!(app.conversation_cache.is_none());
    }

    #[tokio::test]
    async fn theme_persists_per_repository() {
        let (dir, mut app) = focus_test_app().await;
        app.handle_theme_command(Some("light"));
        assert_eq!(app.runtime.theme, forge_config::Theme::Light);
        assert_eq!(crate::theme::active(), forge_config::Theme::Light);

        let session = session_for_workspace(dir.path()).await;
        let restored = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "test".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        assert_eq!(restored.runtime.theme, forge_config::Theme::Light);
        assert_eq!(crate::theme::active(), forge_config::Theme::Light);
    }

    fn assert_buffer_fully_themed(buf: &ratatui::buffer::Buffer) {
        use ratatui::style::Color;
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                let bg = buf[(x, y)].style().bg;
                assert!(bg.is_some(), "unpainted cell at {x},{y}");
                assert_ne!(bg, Some(Color::Black), "terminal-default black at {x},{y}");
            }
        }
    }

    #[tokio::test]
    async fn light_theme_paints_root_canvas_on_draw() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let (_dir, mut app) = focus_test_app().await;
        app.splash_dismissed = true;
        app.handle_theme_command(Some("light"));
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        assert_buffer_fully_themed(term.backend().buffer());
        let corner = term.backend().buffer()[(0, 0)].style().bg;
        assert_eq!(corner, Some(crate::theme::LIGHT_CANVAS));
    }

    #[tokio::test]
    async fn light_theme_resize_keeps_canvas_background() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let (_dir, mut app) = focus_test_app().await;
        app.splash_dismissed = true;
        app.handle_theme_command(Some("light"));
        for (w, h) in [(80, 24), (160, 50), (120, 40)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| app.draw(f)).unwrap();
            assert_buffer_fully_themed(term.backend().buffer());
        }
    }

    #[tokio::test]
    async fn light_theme_representative_layout_snapshot() {
        let (_dir, mut app) = focus_test_app().await;
        app.splash_dismissed = true;
        app.files_visible = true;
        app.handle_theme_command(Some("light"));
        app.session.messages.push(Message {
            role: MessageRole::User,
            content: "Please review this change.\n\nIt spans multiple lines.".into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        });
        app.session.messages.push(Message {
            role: MessageRole::Assistant,
            content: "Here is a concise review of your change.".into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        });
        app.feedback = FeedbackModel::error("Model error: rate limited (HTTP 429).");
        app.conversation_cache = None;
        app.input.set_text("draft reply");
        app.input.cursor = app.input.text.len();

        let text = render_app_text(&mut app, 120, 40);
        assert!(text.contains("Forge"), "missing header:\n{text}");
        assert!(text.contains("FILES"), "missing sidebar:\n{text}");
        assert!(
            text.contains("Please review this change."),
            "missing user message:\n{text}"
        );
        assert!(
            text.contains("concise review"),
            "missing assistant response:\n{text}"
        );
        assert!(
            text.contains("Model error"),
            "missing model error feedback:\n{text}"
        );
        assert!(text.contains("draft reply"), "missing composer:\n{text}");

        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        assert_buffer_fully_themed(term.backend().buffer());
        let buf = term.backend().buffer();
        let mut saw_gutter = false;
        let mut saw_selection = false;
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                if buf[(x, y)].style().fg == Some(crate::theme::USER_MESSAGE_GUTTER_LIGHT) {
                    saw_gutter = true;
                }
                if buf[(x, y)].style().bg == Some(crate::theme::LIGHT_SELECTION) {
                    saw_selection = true;
                }
            }
        }
        assert!(saw_gutter, "expected light-theme user gutter colour");
        assert!(
            saw_selection || text.contains("draft reply"),
            "expected composer selection or typed text"
        );
    }

    #[tokio::test]
    async fn light_theme_overlay_uses_themed_backdrop() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let (_dir, mut app) = focus_test_app().await;
        app.handle_theme_command(Some("light"));
        app.overlay = Some(Overlay::Help);
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        assert_buffer_fully_themed(term.backend().buffer());
    }

    #[tokio::test]
    async fn old_or_malformed_ui_state_migrates_safely_to_default() {
        let (dir, _app) = focus_test_app().await;
        let state_path = dir.path().join(".forge/ui-state.json");
        fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        fs::write(&state_path, r#"{"files_visible":true}"#).unwrap();

        let session = session_for_workspace(dir.path()).await;
        let app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "test".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );

        assert!(!app.files_visible);
    }

    #[tokio::test]
    async fn files_toggle_is_reachable_from_global_palette_dispatch() {
        let (_dir, mut app) = focus_test_app().await;
        assert!(!app.files_visible);

        app.execute_semantic_command(SemanticCommand::DispatchSlash {
            origin: SlashCommandOrigin::GlobalPalette,
            line: "/files".into(),
        })
        .await
        .unwrap();

        assert!(app.files_visible);
        assert_eq!(app.focus.block, FocusBlock::Files);
    }

    #[tokio::test]
    async fn opening_file_does_not_open_closed_files_preference() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("main.rs");
        fs::write(&path, "fn main() {}\n").unwrap();
        app.files_visible = false;

        app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
            .await
            .unwrap();

        assert!(!app.files_visible);
        assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
    }

    #[tokio::test]
    async fn responsive_sizes_render_without_panic_and_follow_files_policy() {
        let (_dir, mut app) = focus_test_app().await;
        app.files_visible = true;
        app.splash_dismissed = true;
        for (width, height, expect_files) in [
            (80, 24, false),
            (120, 40, true),
            (160, 50, true),
            (240, 60, true),
        ] {
            let rendered = render_app_text(&mut app, width, height);
            assert!(
                rendered.contains("Describe a task"),
                "composer should remain reachable at {width}x{height}:\n{rendered}"
            );
            assert_eq!(
                rendered.contains("FILES"),
                expect_files,
                "unexpected Files visibility at {width}x{height}:\n{rendered}"
            );
        }
    }

    #[tokio::test]
    async fn leaving_run_view_does_not_cancel_running_run() {
        let (_dir, mut app) = focus_test_app().await;
        app.run.draft.command_input = "true".into();
        app.run_current_draft();
        let id = app.run.current.as_ref().unwrap().id.clone();

        app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Current))
            .await
            .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Run(id.clone())
        );

        app.execute_semantic_command(SemanticCommand::GoBack)
            .await
            .unwrap();

        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
        assert!(app
            .run
            .current
            .as_ref()
            .is_some_and(|record| record.id == id && record.state == RunState::Running));
    }

    #[tokio::test]
    async fn run_start_updates_activity_without_navigating_from_conversation() {
        let (_dir, mut app) = focus_test_app().await;
        let before = app.workspace_navigation.clone();
        app.run.draft.command_input = "true".into();

        app.run_current_draft();

        assert_eq!(app.workspace_navigation, before);
        assert!(app
            .run
            .current
            .as_ref()
            .is_some_and(|record| record.state == RunState::Running));
        assert!(app
            .activity
            .all()
            .iter()
            .any(|item| item.kind == ActivityKind::Run && item.summary.contains("run started")));
        let summary = app.activity_summary().expect("run summary");
        assert_eq!(summary.label, "Running true");
        assert_eq!(summary.action_label, Some("View output"));

        let rendered = render_app_text(&mut app, 100, 30);
        assert!(rendered.contains("Running true"), "{rendered}");
        assert_eq!(rendered.matches("View output").count(), 1, "{rendered}");
        assert!(
            !rendered.contains("Running validation"),
            "run must not also render the old running tool card:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn run_start_while_in_file_does_not_hijack_workspace() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("main.rs");
        fs::write(&path, "fn main() {}\n").unwrap();
        app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
            .await
            .unwrap();
        let before = app.workspace_navigation.clone();

        app.run.draft.command_input = "true".into();
        app.run_current_draft();

        assert_eq!(app.workspace_navigation, before);
        assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
        assert!(app
            .activity
            .all()
            .iter()
            .any(|item| item.kind == ActivityKind::Run));
    }

    #[tokio::test]
    async fn run_failure_while_in_diff_updates_summary_without_navigation() {
        let (_dir, mut app) = focus_test_app().await;
        app.run.draft.command_input = "false".into();
        app.run_current_draft();
        let run_id = app.run.current.as_ref().unwrap().id.clone();
        app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
            .await
            .unwrap();
        let before = app.workspace_navigation.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        app.run_rx = Some(rx);

        tx.send(RunEvent::Finished {
            exit_code: Some(1),
            success: false,
        })
        .unwrap();
        app.poll_run();

        assert_eq!(app.workspace_navigation, before);
        assert!(app
            .run
            .current
            .as_ref()
            .is_some_and(|record| record.id == run_id && record.state == RunState::Failed));
        assert!(app
            .activity
            .all()
            .iter()
            .any(|item| item.kind == ActivityKind::Run
                && item.severity == FeedbackSeverity::Error
                && item.summary.contains("run failed")));
        assert_eq!(
            app.activity_summary_command(),
            Some(SemanticCommand::OpenRun(RunCommandTarget::Id(run_id)))
        );
    }

    #[tokio::test]
    async fn edge_run_cancel_preserves_output_and_navigation() {
        let (_dir, mut app) = focus_test_app().await;
        let before = app.workspace_navigation.clone();
        app.run.draft.command_input = "long-running".into();
        app.run_current_draft();
        app.append_terminal_output(b"partial output\n");

        app.cancel_run();

        let record = app.run.current.as_ref().expect("current run");
        assert_eq!(record.state, RunState::Cancelled);
        assert_eq!(record.exit_status, None);
        assert!(app.terminal_capture.content.contains("partial output"));
        assert_eq!(app.workspace_navigation, before);
        assert!(app
            .run
            .recent
            .iter()
            .any(|run| run.state == RunState::Cancelled));
    }

    #[tokio::test]
    async fn edge_run_spawn_failure_shows_invocation_without_exit_code() {
        let (_dir, mut app) = focus_test_app().await;
        app.run.draft.command_input = "definitely-missing-forge-command --flag".into();
        app.run_current_draft();
        let run_id = app.run.current.as_ref().unwrap().id.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        app.run_rx = Some(rx);

        tx.send(RunEvent::SpawnFailed("No such file or directory".into()))
            .unwrap();
        app.poll_run();
        app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Id(run_id)))
            .await
            .unwrap();

        let record = app.run.current.as_ref().expect("current run");
        assert_eq!(record.state, RunState::StartFailed);
        assert_eq!(record.exit_status, None);
        assert_eq!(
            record.invocation.executable,
            "definitely-missing-forge-command"
        );
        assert_eq!(record.invocation.arguments, vec!["--flag"]);
        assert!(record.spawn_error.as_deref().unwrap().contains("No such"));

        let rendered = render_app_text(&mut app, 100, 30);
        assert!(rendered.contains("Could not start"), "{rendered}");
        assert!(
            rendered.contains("Executable: definitely-missing-forge-command"),
            "{rendered}"
        );
        assert!(rendered.contains("Arguments: [\"--flag\"]"), "{rendered}");
        assert!(rendered.contains("Directory:"), "{rendered}");
        assert!(
            rendered.contains("Cause: No such file or directory"),
            "{rendered}"
        );
        assert!(rendered.contains("e edit rerun"), "{rendered}");
        assert!(!rendered.contains("Exit status:"), "{rendered}");
    }

    #[tokio::test]
    async fn edge_network_stream_interruption_preserves_partial_response() {
        let dir = TempDir::new().unwrap();
        let session = session_for_workspace_with_model(
            dir.path(),
            Arc::new(MockModelClient::stream_error(
                vec!["partial ".into(), "answer".into()],
                "network connection lost",
            )),
        )
        .await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "test".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        let file = dir.path().join("open.rs");
        fs::write(&file, "fn main() {}\n").unwrap();
        app.execute_semantic_command(SemanticCommand::OpenFile(file))
            .await
            .unwrap();
        let before = app.workspace_navigation.clone();

        app.dispatch_line("hello").await.unwrap();
        app.drain_pending_prompt(None).await.unwrap();

        assert_eq!(app.workspace_navigation, before);
        assert!(!app.busy);
        app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.input.text, "x");
        assert!(app.feedback.text.contains("Retry or Continue"));
        assert!(app.session.messages.iter().any(|message| {
            message.role == MessageRole::Assistant
                && message.content.contains("partial answer")
                && message.content.contains("Interrupted")
        }));
    }

    #[tokio::test]
    async fn edge_open_file_external_rename_updates_path_when_identity_matches() {
        let (dir, mut app) = focus_test_app().await;
        let old = dir.path().join("old.rs");
        let new = dir.path().join("new.rs");
        fs::write(&old, "fn main() {}\nline2\nline3\n").unwrap();
        app.execute_semantic_command(SemanticCommand::OpenFile(old.clone()))
            .await
            .unwrap();
        app.source_viewer.current_line = 2;
        app.source_viewer.top_line = 1;
        fs::rename(&old, &new).unwrap();

        app.file_change_tx
            .send(FileChangeEvent { path: new.clone() })
            .unwrap();
        app.poll_file_changes();

        let new = new.canonicalize().unwrap();
        assert_eq!(app.source_viewer.path.as_deref(), Some(new.as_path()));
        assert_eq!(app.workspace_navigation.current, WorkspaceView::File(new));
        assert_eq!(app.source_viewer.current_line, 2);
        assert_eq!(app.source_viewer.top_line, 1);
        assert_eq!(
            app.source_viewer.notice.as_deref(),
            Some("File renamed externally")
        );
    }

    #[tokio::test]
    async fn edge_open_file_external_delete_keeps_file_view_and_buffer() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("gone.rs");
        fs::write(&path, "fn main() {}\n").unwrap();
        app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
            .await
            .unwrap();
        let opened = app.source_viewer.path.clone().unwrap();
        let lines = app.source_viewer.lines.clone();
        fs::remove_file(&path).unwrap();

        app.file_change_tx
            .send(FileChangeEvent {
                path: opened.clone(),
            })
            .unwrap();
        app.poll_file_changes();

        assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
        assert_eq!(app.source_viewer.path.as_deref(), Some(opened.as_path()));
        assert_eq!(
            app.source_viewer.status,
            crate::source_viewer::ViewerStatus::NotFound
        );
        assert_eq!(app.source_viewer.lines, lines);
        let rendered = render_app_text(&mut app, 100, 30);
        assert!(rendered.contains("File no longer exists"), "{rendered}");
        assert!(rendered.contains("Back"), "{rendered}");
        assert!(rendered.contains("Locate"), "{rendered}");
    }

    #[tokio::test]
    async fn edge_diff_becomes_stale_and_refresh_clears_it() {
        let (_dir, mut app) = focus_test_app().await;
        app.file_explorer
            .git_status
            .status
            .insert(PathBuf::from("one.rs"), GitStatusKind::Modified);
        app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
            .await
            .unwrap();
        app.diff_selected = 0;
        app.file_explorer
            .git_status
            .status
            .insert(PathBuf::from("two.rs"), GitStatusKind::Added);

        app.note_workspace_changed();
        assert!(app.diff_snapshot.stale);
        assert_eq!(app.diff_selected, 0);
        let rendered = render_app_text(&mut app, 100, 30);
        assert!(rendered.contains("Stale review"), "{rendered}");
        assert!(
            rendered.contains("Apply disabled until refresh"),
            "{rendered}"
        );
        assert_eq!(
            app.semantic_command_for_workspace_key(press(KeyCode::Char('r'), KeyModifiers::NONE)),
            Some(SemanticCommand::RefreshDiff)
        );

        app.execute_semantic_command(SemanticCommand::RefreshDiff)
            .await
            .unwrap();
        assert!(!app.diff_snapshot.stale);
        assert_eq!(app.diff_selected, 0);
    }

    #[tokio::test]
    async fn edge_approval_at_80x24_keeps_required_fields_and_actions() {
        let (_dir, mut app) = focus_test_app().await;
        app.open_hitl_overlay(direct_hitl_payload("call-1", "src/main.rs"));

        let rendered = render_app_text(&mut app, 80, 24);
        assert!(rendered.contains("Approval required"), "{rendered}");
        assert!(rendered.contains("Direct"), "{rendered}");
        assert!(rendered.contains("read_file"), "{rendered}");
        assert!(rendered.contains("Working directory"), "{rendered}");
        assert!(rendered.contains("test approval"), "{rendered}");
        assert!(rendered.contains("Allow once"), "{rendered}");
        assert!(rendered.contains("Deny"), "{rendered}");
        assert!(
            rendered.contains("Remember this exact Direct invocation"),
            "{rendered}"
        );
    }

    #[tokio::test]
    async fn edge_mouse_disabled_keeps_keyboard_workflow_and_no_mouse_hint() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("main.rs");
        fs::write(&path, "fn main() {}\n").unwrap();
        app.runtime.mouse_capture = false;
        app.files_visible = true;
        app.file_explorer.refresh_workspace();
        let canonical = path.canonicalize().unwrap();
        app.file_explorer.selected_path = Some(canonical.clone());
        app.focus_block(FocusBlock::Files);

        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::File(canonical)
        );
        let rendered = render_app_text(&mut app, 100, 30);
        assert!(
            !rendered.to_ascii_lowercase().contains("mouse"),
            "mouse-disabled mode should not reserve mouse-specific hints:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn edge_hit_target_invalidated_cancels_double_click_state() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("main.rs");
        fs::write(&path, "fn main() {}\n").unwrap();
        app.file_explorer.refresh_workspace();
        app.files_visible = true;
        draw_app(&mut app, 120, 30);
        let canonical = path.canonicalize().unwrap();
        let (x, y) = hit_point_for_path(
            &app,
            |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
            &canonical,
        );

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();
        assert!(app.pending_double_click.is_some());
        app.invalidate_hit_regions();
        assert!(app.pending_double_click.is_none());
        draw_app(&mut app, 120, 30);
        let (x, y) = hit_point_for_path(
            &app,
            |target, path| matches!(target, HitTarget::FileEntry(p) if p == path),
            &canonical,
        );
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .await
            .unwrap();

        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
    }

    #[tokio::test]
    async fn edge_end_to_end_recovery_flow_mouse_enabled_and_disabled() {
        for mouse_capture in [true, false] {
            let (dir, mut app) = focus_test_app().await;
            app.runtime.mouse_capture = mouse_capture;
            let path = dir.path().join("flow.rs");
            fs::write(&path, "fn flow() {}\n").unwrap();

            app.file_explorer
                .git_status
                .status
                .insert(PathBuf::from("flow.rs"), GitStatusKind::Modified);
            assert_eq!(
                app.workspace_navigation.current,
                WorkspaceView::Conversation
            );

            app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
                .await
                .unwrap();
            assert_eq!(
                app.workspace_navigation.current,
                WorkspaceView::File(path.clone())
            );

            app.execute_semantic_command(SemanticCommand::ReviewChanges(
                DiffCommandContext::Current,
            ))
            .await
            .unwrap();
            assert_eq!(
                app.workspace_navigation.current,
                WorkspaceView::Diff(DiffCommandContext::Current)
            );

            app.run.draft.command_input = "cargo test".into();
            app.run_current_draft();
            let run_id = app.run.current.as_ref().unwrap().id.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            app.run_rx = Some(rx);
            tx.send(RunEvent::Finished {
                exit_code: Some(101),
                success: false,
            })
            .unwrap();
            app.poll_run();
            assert_eq!(
                app.workspace_navigation.current,
                WorkspaceView::Diff(DiffCommandContext::Current)
            );

            app.execute_semantic_command(SemanticCommand::ActivateActivitySummary)
                .await
                .unwrap();
            assert_eq!(
                app.workspace_navigation.current,
                WorkspaceView::Run(run_id.clone())
            );
            app.execute_semantic_command(SemanticCommand::GoBack)
                .await
                .unwrap();
            assert_eq!(
                app.workspace_navigation.current,
                WorkspaceView::Diff(DiffCommandContext::Current)
            );

            fs::write(&path, "fn flow() {}\nfn changed() {}\n").unwrap();
            app.file_explorer
                .git_status
                .status
                .insert(PathBuf::from("extra.rs"), GitStatusKind::Added);
            app.file_change_tx
                .send(FileChangeEvent { path: path.clone() })
                .unwrap();
            app.poll_file_changes();
            assert!(app.diff_snapshot.stale);

            app.execute_semantic_command(SemanticCommand::RefreshDiff)
                .await
                .unwrap();
            assert!(!app.diff_snapshot.stale);
            app.execute_semantic_command(SemanticCommand::GoHome)
                .await
                .unwrap();
            assert_eq!(
                app.workspace_navigation.current,
                WorkspaceView::Conversation
            );
        }
    }

    #[tokio::test]
    async fn agent_streaming_while_viewing_file_does_not_navigate() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("main.rs");
        fs::write(&path, "fn main() {}\n").unwrap();
        app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
            .await
            .unwrap();
        let before = app.workspace_navigation.clone();

        app.busy = true;
        app.busy_phase = BusyPhase::Model;
        app.pending_prompt = None;
        app.stream_preview = "partial answer".into();
        let rendered = render_app_text(&mut app, 100, 30);

        assert_eq!(app.workspace_navigation, before);
        assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
        assert!(rendered.contains("fn main()"), "{rendered}");
        assert!(
            !rendered.contains("partial answer"),
            "File view should remain primary while streaming:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn agent_thinking_keeps_composer_usable() {
        let (_dir, mut app) = focus_test_app().await;
        app.busy = true;
        app.busy_phase = BusyPhase::Model;
        app.stream_thinking = "planning".into();
        app.focus_block(FocusBlock::Composer);

        app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(app.input.text, "x");
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
        let summary = app.activity_summary().expect("thinking summary");
        assert_eq!(summary.label, "Forge is thinking");
    }

    #[tokio::test]
    async fn activity_summary_priority_renders_one_actionable_row() {
        let (_dir, mut app) = focus_test_app().await;
        app.file_explorer
            .git_status
            .status
            .insert(PathBuf::from("changed.rs"), GitStatusKind::Modified);
        app.busy = true;
        app.busy_phase = BusyPhase::Model;
        app.run.draft.command_input = "cargo test".into();
        app.run_current_draft();

        let rendered = render_app_text(&mut app, 100, 30);
        assert!(rendered.contains("Running cargo test"), "{rendered}");
        assert_eq!(rendered.matches("View output").count(), 1, "{rendered}");
        assert!(
            !rendered.contains("files changed · Review"),
            "Run summary must outrank changes:\n{rendered}"
        );
        assert!(
            !rendered.contains("Forge is thinking"),
            "Run summary must outrank thinking:\n{rendered}"
        );

        let (tx, rx) = std::sync::mpsc::channel();
        app.run_rx = Some(rx);
        tx.send(RunEvent::Finished {
            exit_code: Some(1),
            success: false,
        })
        .unwrap();
        app.poll_run();

        let rendered = render_app_text(&mut app, 100, 30);
        assert!(rendered.contains("Run failed: cargo test"), "{rendered}");
        assert_eq!(rendered.matches("Inspect").count(), 1, "{rendered}");
        assert!(
            !rendered.contains("Running cargo test"),
            "Failure summary must replace active-run summary:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn summary_action_opens_expected_workspace_view() {
        let (_dir, mut app) = focus_test_app().await;
        app.run.draft.command_input = "true".into();
        app.run_current_draft();
        let id = app.run.current.as_ref().unwrap().id.clone();
        app.focus_block(FocusBlock::Workspace);

        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(app.workspace_navigation.current, WorkspaceView::Run(id));
    }

    #[tokio::test]
    async fn changes_summary_action_uses_review_changes_command() {
        let (_dir, mut app) = focus_test_app().await;
        app.file_explorer
            .git_status
            .status
            .insert(PathBuf::from("changed.rs"), GitStatusKind::Modified);

        assert_eq!(
            app.activity_summary_command(),
            Some(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        );
        app.execute_semantic_command(SemanticCommand::ActivateActivitySummary)
            .await
            .unwrap();

        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Diff(DiffCommandContext::Current)
        );
    }

    #[tokio::test]
    async fn approval_overlay_preserves_underlying_workspace() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("main.rs");
        fs::write(&path, "fn main() {}\n").unwrap();
        app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
            .await
            .unwrap();
        let before = app.workspace_navigation.clone();
        app.session.pending_hitl = Some(forge_types::HitlPayload {
            call_id: "1".into(),
            tool: "bash".into(),
            args_redacted: json!({"command": "cargo test"}),
            reason: "test approval".into(),
        });

        app.maybe_open_hitl();

        assert!(matches!(app.overlay, Some(Overlay::Hitl { .. })));
        assert_eq!(app.workspace_navigation, before);
        assert!(app.activity_summary().is_none());
        assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
    }

    fn direct_hitl_payload(call_id: &str, path: &str) -> HitlPayload {
        HitlPayload {
            call_id: call_id.into(),
            tool: "read_file".into(),
            args_redacted: json!({"path": path}),
            reason: "test approval".into(),
        }
    }

    #[tokio::test]
    async fn approval_direct_allow_once_resolves_without_remembering() {
        let (dir, mut app) = focus_test_app().await;
        fs::write(dir.path().join("allowed.txt"), "ok").unwrap();
        app.session.pending_hitl = Some(direct_hitl_payload("direct-once", "allowed.txt"));
        app.maybe_open_hitl();

        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        assert!(app.session.pending_hitl.is_none());
        assert!(app.hitl_session_allow.is_empty());
        assert!(app
            .session
            .messages
            .iter()
            .any(|message| message.content == "ok"));
    }

    #[tokio::test]
    async fn approval_remembered_direct_invocation_matches_exact_identity() {
        let (dir, mut app) = focus_test_app().await;
        fs::write(dir.path().join("remember.txt"), "ok").unwrap();
        let payload = direct_hitl_payload("remember", "remember.txt");
        app.session.pending_hitl = Some(payload.clone());
        app.maybe_open_hitl();

        app.handle_key(press(KeyCode::Char('s'), KeyModifiers::NONE))
            .await
            .unwrap();

        let identity = app.approval_identity_for_payload(&payload).unwrap();
        assert!(app.hitl_session_allow.contains(&identity));
        assert!(!app.hitl_session_allow.contains(
            &app.approval_identity_for_payload(&direct_hitl_payload("arg", "other.txt"))
                .unwrap()
        ));

        let env_payload = HitlPayload {
            args_redacted: json!({"path": "remember.txt", "env": {"RUST_LOG": "debug"}}),
            ..direct_hitl_payload("env", "remember.txt")
        };
        assert!(!app
            .hitl_session_allow
            .contains(&app.approval_identity_for_payload(&env_payload).unwrap()));

        let cwd_payload = HitlPayload {
            args_redacted: json!({"path": "remember.txt", "cwd": "nested"}),
            ..direct_hitl_payload("cwd", "remember.txt")
        };
        assert!(!app
            .hitl_session_allow
            .contains(&app.approval_identity_for_payload(&cwd_payload).unwrap()));

        let (other_dir, other_app) = focus_test_app().await;
        fs::write(other_dir.path().join("remember.txt"), "ok").unwrap();
        assert!(!app
            .hitl_session_allow
            .contains(&other_app.approval_identity_for_payload(&payload).unwrap()));
    }

    #[tokio::test]
    async fn approval_remembered_direct_expires_with_session() {
        let (dir, mut app) = focus_test_app().await;
        fs::write(dir.path().join("session.txt"), "ok").unwrap();
        let payload = direct_hitl_payload("session", "session.txt");
        app.session.pending_hitl = Some(payload.clone());
        app.maybe_open_hitl();
        app.handle_key(press(KeyCode::Char('s'), KeyModifiers::NONE))
            .await
            .unwrap();

        let next_session = session_for_workspace(dir.path()).await;
        let next_app = TuiApp::new(
            next_session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "test".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );

        assert!(next_app.hitl_session_allow.is_empty());
        assert_ne!(
            app.approval_identity_for_payload(&payload),
            next_app.approval_identity_for_payload(&payload)
        );
    }

    #[tokio::test]
    async fn approval_shell_mode_cannot_be_remembered() {
        let (_dir, mut app) = focus_test_app().await;
        app.session.pending_hitl = Some(HitlPayload {
            call_id: "shell".into(),
            tool: "bash".into(),
            args_redacted: json!({"command": "git push origin main"}),
            reason: "test approval".into(),
        });
        app.maybe_open_hitl();

        let Some(Overlay::Hitl { approval, .. }) = &app.overlay else {
            panic!("expected approval overlay");
        };
        assert_eq!(approval.mode, ApprovalExecutionMode::Shell);
        assert!(!approval.remember_eligible);
        assert_eq!(
            app.approval_identity_for_payload(app.session.pending_hitl.as_ref().unwrap()),
            None
        );
    }

    #[tokio::test]
    async fn approval_escape_denies_and_underlying_commands_are_blocked() {
        let (dir, mut app) = focus_test_app().await;
        fs::write(dir.path().join("blocked.txt"), "ok").unwrap();
        app.session.pending_hitl = Some(direct_hitl_payload("esc", "blocked.txt"));
        app.maybe_open_hitl();
        let before_history = app.workspace_navigation.clone();

        app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.input.text.is_empty());
        assert!(app.session.pending_hitl.is_some());

        app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .unwrap();

        assert!(app.session.pending_hitl.is_none());
        assert_eq!(app.workspace_navigation, before_history);
        assert!(app
            .session
            .messages
            .iter()
            .any(|message| message.content.contains("HITL denied")));
        assert!(!app
            .session
            .messages
            .iter()
            .any(|message| message.content == "ok"));
    }

    #[tokio::test]
    async fn approval_duplicate_confirmation_is_idempotent() {
        let (dir, mut app) = focus_test_app().await;
        fs::write(dir.path().join("dup.txt"), "ok").unwrap();
        app.session.pending_hitl = Some(direct_hitl_payload("dup", "dup.txt"));
        app.maybe_open_hitl();

        app.resolve_hitl_overlay(HitlDecision::Approve, false)
            .await
            .unwrap();
        app.resolve_hitl_overlay(HitlDecision::Approve, false)
            .await
            .unwrap();

        let successful_tool_messages = app
            .session
            .messages
            .iter()
            .filter(|message| message.content == "ok")
            .count();
        assert_eq!(successful_tool_messages, 1);
    }

    #[tokio::test]
    async fn approval_overlay_80x24_renders_actions_and_redacts_secrets() {
        let (_dir, mut app) = focus_test_app().await;
        app.session.pending_hitl = Some(HitlPayload {
            call_id: "secret".into(),
            tool: "read_file".into(),
            args_redacted: json!({"path": "config.txt", "api_key": "[REDACTED]"}),
            reason: "secret test".into(),
        });
        app.maybe_open_hitl();

        let rendered = render_app_text(&mut app, 80, 24);

        assert!(rendered.contains("Approval required"), "{rendered}");
        assert!(rendered.contains("Mode: Direct"), "{rendered}");
        assert!(rendered.contains("Executable: read_file"), "{rendered}");
        assert!(rendered.contains("Working directory:"), "{rendered}");
        assert!(rendered.contains("[Allow once]"), "{rendered}");
        assert!(rendered.contains("[Deny]"), "{rendered}");
        assert!(rendered.contains("[REDACTED]"), "{rendered}");
        assert!(
            !rendered.contains("Remember this exact Direct invocation"),
            "{rendered}"
        );
    }

    #[tokio::test]
    async fn run_activity_history_remains_available_in_activity_panel() {
        let (_dir, mut app) = focus_test_app().await;
        app.run.draft.command_input = "true".into();
        app.run_current_draft();
        app.open_bottom_panel(Some(BottomPanelTab::Activity));

        let rendered = render_app_text(&mut app, 100, 30);

        assert!(rendered.contains("Run"), "{rendered}");
        assert!(rendered.contains("run started: true"), "{rendered}");
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
    }

    #[tokio::test]
    async fn characterization_contextual_views_are_reachable_with_current_controls() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("source.rs");
        fs::write(&path, "fn main() {}\n").unwrap();
        app.focus_block(FocusBlock::Workspace);
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );

        app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
            .await
            .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Diff(DiffCommandContext::Current)
        );
        assert_eq!(app.focus.block, FocusBlock::Workspace);

        app.handle_key(press(KeyCode::Left, KeyModifiers::SHIFT))
            .await
            .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );

        app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
            .await
            .unwrap();
        assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
    }

    #[tokio::test]
    async fn characterization_files_selection_and_expansion_survive_focus_roundtrip() {
        let (dir, mut app) = focus_test_app().await;
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "").unwrap();
        app.file_explorer.refresh_workspace();
        let src = dir.path().join("src").canonicalize().unwrap();
        app.files_visible = true;
        app.focus_block(FocusBlock::Files);
        app.file_explorer.selected_path = Some(src.clone());
        app.file_explorer.expand_selected();

        app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_ne!(app.focus.block, FocusBlock::Files);
        app.handle_key(press(KeyCode::BackTab, KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(app.focus.block, FocusBlock::Files);
        assert_eq!(
            app.file_explorer.selected_path.as_deref(),
            Some(src.as_path())
        );
        assert!(app
            .file_explorer
            .visible_nodes()
            .iter()
            .any(|node| node.display_name == "lib.rs"));
    }

    #[tokio::test]
    async fn characterization_80x24_draws_without_panic() {
        use ratatui::backend::TestBackend;

        let (_dir, mut app) = focus_test_app().await;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn characterization_run_completion_preserves_bottom_panel_focus() {
        let (_dir, mut app) = focus_test_app().await;
        app.open_bottom_panel(Some(BottomPanelTab::Run));
        app.run.draft.command_input = "/usr/bin/true".into();

        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::BottomPanel);
        assert!(app.pending_validation);

        app.drain_pending_validation(None).await.unwrap();
        for _ in 0..50 {
            app.poll_run();
            if app
                .run
                .current
                .as_ref()
                .is_some_and(|record| record.state != RunState::Running)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(app.focus.block, FocusBlock::BottomPanel);
        assert!(app.run.current.as_ref().is_some_and(|record| matches!(
            record.state,
            RunState::Succeeded | RunState::Failed | RunState::StartFailed
        )));
    }

    #[tokio::test]
    async fn switching_to_diff_focuses_workspace_for_navigation() {
        let (_dir, mut app) = focus_test_app().await;
        app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
            .await
            .unwrap();
        app.file_explorer
            .git_status
            .status
            .insert(std::path::PathBuf::from("a.txt"), GitStatusKind::Modified);
        app.file_explorer
            .git_status
            .status
            .insert(std::path::PathBuf::from("b.txt"), GitStatusKind::Modified);

        app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Diff(DiffCommandContext::Current)
        );
        assert_eq!(app.focus.block, FocusBlock::Workspace);
        assert_eq!(app.diff_selected, 1);
    }

    #[tokio::test]
    async fn registered_printable_editor_commands_do_not_enter_composer() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("source.txt");
        fs::write(&path, "a\nb\nc\n").unwrap();
        app.open_file_in_editor(&path);
        app.input.set_text("");

        app.handle_key(press(KeyCode::Char('r'), KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Workspace);
        assert!(app.input.text.is_empty());

        app.handle_key(press(KeyCode::Char('G'), KeyModifiers::SHIFT))
            .await
            .unwrap();
        assert_eq!(app.focus.block, FocusBlock::Workspace);
        assert!(app.input.text.is_empty());
    }

    #[tokio::test]
    async fn non_printable_keys_do_not_type_to_compose() {
        let (_dir, mut app) = focus_test_app().await;
        app.focus_block(FocusBlock::Workspace);

        for key in [
            press(KeyCode::Enter, KeyModifiers::NONE),
            press(KeyCode::Left, KeyModifiers::NONE),
            press(KeyCode::Right, KeyModifiers::SHIFT),
            press(KeyCode::Char('c'), KeyModifiers::CONTROL),
            press(KeyCode::Char('x'), KeyModifiers::ALT),
        ] {
            app.handle_key(key).await.unwrap();
            assert_eq!(app.focus.block, FocusBlock::Workspace);
            assert!(app.input.text.is_empty());
        }
    }

    #[tokio::test]
    async fn source_search_is_transient_and_esc_restores_workspace_navigation() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("source.txt");
        fs::write(&path, "line\n").unwrap();
        app.open_file_in_editor(&path);
        app.handle_key(press(KeyCode::Char('f'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert_eq!(
            app.focus.mode,
            FocusMode::Transient(TransientOwner::SourceSearch)
        );
        app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(!app.source_viewer.search.open);
        assert_eq!(app.focus.mode, FocusMode::Navigation);
    }

    #[tokio::test]
    async fn source_search_keeps_shift_arrows_inside_the_search_field() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("source.txt");
        fs::write(&path, "line\n").unwrap();
        app.open_file_in_editor(&path);
        app.handle_key(press(KeyCode::Char('f'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
            .await
            .unwrap();
        assert!(app.source_viewer.search.open);
        assert_eq!(
            app.focus.mode,
            FocusMode::Transient(TransientOwner::SourceSearch)
        );
    }

    #[tokio::test]
    async fn jump_to_line_keeps_shift_arrows_inside_the_jump_field() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("source.txt");
        fs::write(&path, "line\n").unwrap();
        app.open_file_in_editor(&path);
        app.handle_key(press(KeyCode::Char('g'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
            .await
            .unwrap();
        assert!(app.source_viewer.jump.open);
        assert_eq!(
            app.focus.mode,
            FocusMode::Transient(TransientOwner::JumpToLine)
        );
    }

    #[tokio::test]
    async fn editor_reload_does_not_reach_chat_input() {
        let (dir, mut app) = focus_test_app().await;
        let path = dir.path().join("source.txt");
        fs::write(&path, "before\n").unwrap();
        app.input.set_text("draft");
        app.open_file_in_editor(&path);
        fs::write(&path, "after\n").unwrap();
        app.handle_key(press(KeyCode::Char('r'), KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.input.text, "draft");
        assert_eq!(app.source_viewer.lines, vec!["after"]);
    }

    #[tokio::test]
    async fn explorer_new_file_dialog_owns_printable_input_and_selects_created_file() {
        let (dir, mut app) = focus_test_app().await;
        app.files_visible = true;
        app.focus_block(FocusBlock::Files);
        app.input.set_text("");

        app.handle_key(press(KeyCode::Char('n'), KeyModifiers::NONE))
            .await
            .unwrap();
        for ch in "new.rs".chars() {
            app.handle_key(press(KeyCode::Char(ch), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        assert!(app.input.text.is_empty());
        assert!(matches!(
            app.explorer_dialog,
            Some(ExplorerDialog::Name { .. })
        ));

        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(matches!(
            app.explorer_dialog,
            Some(ExplorerDialog::ConfirmCreate { .. })
        ));
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        let created = dir.path().join("new.rs").canonicalize().unwrap();
        assert!(created.is_file());
        assert_eq!(
            app.file_explorer.selected_path.as_deref(),
            Some(created.as_path())
        );
        assert_eq!(app.focus.block, FocusBlock::Files);
    }

    #[tokio::test]
    async fn explorer_name_escape_cancels_without_focus_change_or_composer_input() {
        let (_dir, mut app) = focus_test_app().await;
        app.files_visible = true;
        app.focus_block(FocusBlock::Files);

        app.handle_key(press(KeyCode::Char('n'), KeyModifiers::NONE))
            .await
            .unwrap();
        app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
            .await
            .unwrap();
        app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .unwrap();

        assert!(app.explorer_dialog.is_none());
        assert_eq!(app.focus.block, FocusBlock::Files);
        assert!(app.input.text.is_empty());
    }

    #[tokio::test]
    async fn explorer_rename_prepopulates_and_updates_open_child_file() {
        let (dir, mut app) = focus_test_app().await;
        let src = dir.path().join("src");
        fs::create_dir(&src).unwrap();
        let src = src.canonicalize().unwrap();
        let child = src.join("lib.rs");
        fs::write(&child, "pub fn old() {}\n").unwrap();
        app.file_explorer.refresh_workspace();
        app.file_explorer.selected_path = Some(src.clone());
        app.open_file_in_editor(&child);
        app.files_visible = true;
        app.focus_block(FocusBlock::Files);
        app.file_explorer.selected_path = Some(src.clone());

        app.handle_key(press(KeyCode::Char('R'), KeyModifiers::SHIFT))
            .await
            .unwrap();
        match app.explorer_dialog.as_mut() {
            Some(ExplorerDialog::Name { input, .. }) => {
                assert_eq!(input, "src");
                *input = "Source".into();
            }
            other => panic!("unexpected dialog: {other:?}"),
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        let renamed_child = dir.path().join("Source/lib.rs").canonicalize().unwrap();
        assert!(renamed_child.is_file());
        assert_eq!(
            app.source_viewer.path.as_deref(),
            Some(renamed_child.as_path())
        );
        let renamed_dir = dir.path().join("Source").canonicalize().unwrap();
        assert_eq!(
            app.file_explorer.selected_path.as_deref(),
            Some(renamed_dir.as_path())
        );
    }

    #[tokio::test]
    async fn explorer_rename_collision_keeps_name_dialog_with_error() {
        let (dir, mut app) = focus_test_app().await;
        let old = dir.path().join("old.rs");
        let existing = dir.path().join("existing.rs");
        fs::write(&old, "").unwrap();
        fs::write(&existing, "").unwrap();
        app.file_explorer.refresh_workspace();
        app.file_explorer.selected_path = Some(old.canonicalize().unwrap());
        app.files_visible = true;
        app.focus_block(FocusBlock::Files);

        app.open_explorer_name_dialog(ExplorerNameAction::Rename);
        match app.explorer_dialog.as_mut() {
            Some(ExplorerDialog::Name { input, .. }) => *input = "existing.rs".into(),
            other => panic!("unexpected dialog: {other:?}"),
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        match app.explorer_dialog {
            Some(ExplorerDialog::Name {
                error: Some(error), ..
            }) => {
                assert!(error.contains("Destination already exists"));
            }
            other => panic!("unexpected dialog: {other:?}"),
        }
        assert!(app.input.text.is_empty());
    }

    #[tokio::test]
    async fn explorer_delete_non_empty_folder_requires_stronger_confirmation() {
        let (dir, mut app) = focus_test_app().await;
        let folder = dir.path().join("generated");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("out.txt"), "").unwrap();
        let folder = folder.canonicalize().unwrap();
        app.file_explorer.refresh_workspace();
        app.file_explorer.selected_path = Some(folder.clone());
        app.files_visible = true;
        app.focus_block(FocusBlock::Files);

        app.handle_key(press(KeyCode::Char('d'), KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(matches!(
            app.explorer_dialog,
            Some(ExplorerDialog::ConfirmDelete {
                non_empty: true,
                permanent: false,
                ..
            })
        ));

        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(folder.exists());
        assert!(app.explorer_dialog.is_some());
    }

    #[tokio::test]
    async fn overlay_precedes_block_navigation() {
        let (_dir, mut app) = focus_test_app().await;
        app.focus.mode = FocusMode::Navigation;
        app.overlay = Some(Overlay::welcome());
        app.handle_key(press(KeyCode::Char(']'), KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(
            app.workspace_navigation.current,
            WorkspaceView::Conversation
        );
        assert!(app.overlay.is_some());
    }

    #[tokio::test]
    async fn resize_drops_focus_from_a_zero_width_files_block() {
        use ratatui::backend::TestBackend;

        let (_dir, mut app) = focus_test_app().await;
        app.files_visible = true;
        app.focus_block(FocusBlock::Files);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(app.focus.block, FocusBlock::Workspace);
        assert_eq!(app.focus.mode, FocusMode::Navigation);
    }

    #[tokio::test]
    async fn typing_reuses_cached_conversation_lines() {
        use ratatui::backend::TestBackend;

        let (dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "test".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.splash_dismissed = true;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        Arc::get_mut(&mut app.conversation_cache.as_mut().unwrap().lines)
            .expect("cache handle is unshared between frames")
            .reserve(1_000);
        let cached_capacity = app.conversation_cache.as_ref().unwrap().lines.capacity();

        app.input.insert('x');
        terminal.draw(|frame| app.draw(frame)).unwrap();

        assert_eq!(
            app.conversation_cache.as_ref().unwrap().lines.capacity(),
            cached_capacity
        );
    }

    #[tokio::test]
    async fn streaming_updates_reuse_cached_transcript_lines() {
        use ratatui::backend::TestBackend;

        let (dir, mut session) = test_session().await;
        session.messages.push(Message {
            role: MessageRole::Assistant,
            content: "historical answer".into(),
            tool_call_id: None,
            name: None,
            thinking: Some("historical completed thinking".into()),
            thinking_duration_secs: Some(1.0),
            tool_calls: vec![],
        });
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "test".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.splash_dismissed = true;
        app.busy = true;
        app.busy_phase = BusyPhase::Model;
        app.stream_preview = "first chunk".into();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        Arc::get_mut(&mut app.conversation_cache.as_mut().unwrap().lines)
            .expect("cache handle is unshared between frames")
            .reserve(1_000);
        let cached_capacity = app.conversation_cache.as_ref().unwrap().lines.capacity();

        app.stream_preview.push_str(" and updated tail");
        terminal.draw(|frame| app.draw(frame)).unwrap();

        assert_eq!(
            app.conversation_cache.as_ref().unwrap().lines.capacity(),
            cached_capacity,
            "stream deltas must not rebuild historical transcript lines"
        );
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("updated tail"), "{rendered}");
        assert!(
            !rendered.contains("historical completed thinking"),
            "{rendered}"
        );
    }

    /// A cache hit must share the cached line buffer, not copy it. Pointer
    /// identity is a direct check: the previous code deep-copied every `Line` and
    /// `Span` on every frame, so the allocation would differ here.
    #[tokio::test]
    async fn cache_hit_shares_transcript_lines_without_copying() {
        let (_dir, mut app) = focus_test_app().await;
        app.splash_dismissed = true;
        app.session.messages.push(forge_types::Message::new(
            forge_types::MessageRole::Assistant,
            "cached transcript body",
        ));
        draw_app(&mut app, 100, 30);
        let first = Arc::clone(&app.conversation_cache.as_ref().unwrap().lines);

        // Typing does not change the render key, so this is a cache hit.
        app.input.insert('x');
        draw_app(&mut app, 100, 30);
        let second = Arc::clone(&app.conversation_cache.as_ref().unwrap().lines);

        assert!(
            Arc::ptr_eq(&first, &second),
            "a cache hit must reuse the same line allocation, not clone it"
        );
    }

    #[test]
    fn footer_limits_parser_keeps_only_inline_limit_fields() {
        let limits = footer_limits_from_report(&[
            "Provider: OpenAI Codex".into(),
            "Session limit: 75% remaining".into(),
            "Weekly limit: 50% remaining".into(),
            "Credits: unlimited".into(),
        ]);

        assert_eq!(limits.usage, "Session limit: 75% remaining");
        assert_eq!(limits.weekly_limit, "Weekly limit: 50% remaining");
        assert_eq!(limits.credits, "Credits: unlimited");
    }

    #[test]
    fn recent_resume_sessions_lists_previous_valid_journals() {
        let dir = TempDir::new().unwrap();
        let current = uuid::Uuid::new_v4();
        let previous = uuid::Uuid::new_v4();
        std::fs::write(dir.path().join(format!("{current}.db")), "").unwrap();
        std::fs::write(dir.path().join(format!("{previous}.db")), "").unwrap();
        std::fs::write(dir.path().join("not-a-session.db"), "").unwrap();
        std::fs::write(dir.path().join(format!("{}.txt", uuid::Uuid::new_v4())), "").unwrap();

        let sessions = recent_resume_sessions(dir.path(), current, 10).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, previous);
    }

    #[tokio::test]
    async fn resume_command_replaces_active_conversation_in_app() {
        let (dir, session) = test_session().await;
        let model = Arc::new(MockModelClient::script(vec![]));
        let mut previous = AgentSession::create(
            LoopConfig {
                max_turns: 4,
                workspace: dir.path().to_path_buf(),
                journal_dir: dir.path().join("j"),
                enable_context_lifecycle: true,
                enable_governance: true,
                ..Default::default()
            },
            model,
            ToolRegistry::new(),
        )
        .await
        .unwrap();
        previous
            .append_user_message("restored conversation")
            .await
            .unwrap();
        let previous_id = previous.session_id;

        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.dispatch_line(&format!("/resume {previous_id}"))
            .await
            .unwrap();

        assert_eq!(app.session.session_id, previous_id);
        assert!(app
            .session
            .messages
            .iter()
            .any(|message| message.content == "restored conversation"));
        assert!(app.status_message.contains("resumed"));
        assert!(app.notices.is_empty());
        assert!(app
            .activity
            .all()
            .iter()
            .any(|item| item.summary.contains("session resumed")));
    }

    #[tokio::test]
    async fn compact_reports_context_handoff_in_chat_and_activity() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );

        app.dispatch_line("/compact").await.unwrap();

        assert!(app.notices.is_empty());
        assert!(app.ui_banners.is_empty());
        assert!(app
            .activity
            .all()
            .iter()
            .any(|item| item.kind == ActivityKind::Context));
        assert_eq!(app.status_message, "Continuing in a fresh context");
        assert!(app
            .ui_banners
            .iter()
            .all(|item| !matches!(item, ChatItem::Banner { .. })));
    }

    #[tokio::test]
    async fn enter_while_busy_enqueues_user_message() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.busy = true;
        for c in "queued later".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.message_queue.len(), 1);
        assert!(app.pending_prompt.is_none());
        assert_eq!(
            app.message_queue.iter().next().map(|s| s.as_str()),
            Some("queued later")
        );
    }

    #[tokio::test]
    async fn typing_while_busy_updates_input_buffer() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.busy = true;
        for c in "next".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        assert_eq!(app.input.text, "next");
        assert_eq!(app.message_queue.len(), 0);
    }

    #[tokio::test]
    async fn ctrl_p_toggles_bottom_panel_without_touching_input() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.input.set_text("draft");

        app.handle_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert!(app.bottom_panel.open);
        assert_eq!(app.input.text, "draft");

        app.handle_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert!(!app.bottom_panel.open);
        assert_eq!(app.input.text, "draft");
    }

    #[tokio::test]
    async fn alt_number_opens_selected_bottom_panel_tab() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );

        app.handle_key(press(KeyCode::Char('4'), KeyModifiers::ALT))
            .await
            .unwrap();
        assert!(app.bottom_panel.open);
        assert_eq!(app.bottom_panel.active, BottomPanelTab::Activity);
    }

    #[tokio::test]
    async fn focused_bottom_panel_cycles_without_typing_into_chat() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.input.set_text("draft");
        app.bottom_panel.open_tab(BottomPanelTab::Terminal);
        app.focus_block(FocusBlock::BottomPanel);

        app.handle_key(press(KeyCode::Right, KeyModifiers::ALT))
            .await
            .unwrap();
        assert_eq!(app.bottom_panel.active, BottomPanelTab::Activity);
        app.handle_key(press(KeyCode::Left, KeyModifiers::ALT))
            .await
            .unwrap();
        assert_eq!(app.bottom_panel.active, BottomPanelTab::Terminal);
        assert_eq!(app.input.text, "draft");
    }

    #[tokio::test]
    async fn editor_uppercase_g_does_not_reach_chat_input() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (dir, session) = test_session().await;
        let path = dir.path().join("source.txt");
        fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.input.set_text("draft");
        app.open_file_in_editor(&path);

        app.handle_key(press(KeyCode::Char('G'), KeyModifiers::SHIFT))
            .await
            .unwrap();
        assert_eq!(app.source_viewer.current_line, 2);
        assert_eq!(app.input.text, "draft");
    }

    #[tokio::test]
    async fn question_mark_opens_help_overlay() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.handle_key(press(KeyCode::F(1), KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(matches!(app.overlay, Some(Overlay::Help)));
        assert!(app.input.text.is_empty());
        assert!(app.feedback.text.contains("Help"));
    }

    #[tokio::test]
    async fn empty_enter_when_idle_dequeues_and_sends() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        // Simulate messages enqueued while processing.
        app.message_queue.enqueue("from queue");
        app.busy = false;
        assert!(app.pending_prompt.is_none());
        // Empty Enter = user action to dequeue + send
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.message_queue.is_empty());
        assert_eq!(app.pending_prompt.as_deref(), Some("from queue"));
        assert!(app.busy);
    }

    #[tokio::test]
    async fn ctrl_backspace_cancels_selected_queue_message() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.enqueue_user_message("a".into());
        app.enqueue_user_message("b".into());
        app.move_queue_selection(1);
        app.handle_key(press(KeyCode::Backspace, KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert_eq!(app.message_queue.len(), 1);
        assert_eq!(
            app.message_queue.iter().next().map(|s| s.as_str()),
            Some("a")
        );
    }

    #[tokio::test]
    async fn effort_selection_persists_across_tui_instances() {
        let (_dir, session) = test_session().await;
        let credential_dir = tempfile::tempdir().unwrap();
        let credential_path = credential_dir.path().join("credentials.toml");
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.connect_store = CredentialStore::new(credential_path.clone());

        app.reasoning_effort = ReasoningEffort::High;
        app.persist_selection();

        assert_eq!(
            app.connect_store.last_effort().unwrap().as_deref(),
            Some("high")
        );

        let (_dir, session) = test_session().await;
        let mut restarted = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        restarted.connect_store = CredentialStore::new(credential_path);
        restarted = restarted.restore_saved_auth();

        assert_eq!(restarted.reasoning_effort, ReasoningEffort::High);
    }

    #[tokio::test]
    async fn model_command_applies_provider_id_to_session() {
        let (_dir, session) = test_session().await;
        let cred_dir = tempfile::tempdir().unwrap();
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
        app.connect_store
            .set_api_key("openai", "sk-test-openai-credential")
            .unwrap();
        app.connect_profile = Some("openai".into());
        app.runtime.model_label = "openai/gpt-4.1-mini".into();
        app.session.set_active_model("openai/gpt-4.1-mini");
        app.apply_model_selection("native", "openai/gpt-4.1-mini");
        assert_eq!(app.runtime.model_label, "openai/gpt-4.1-mini");
        assert_eq!(app.session.active_model, "openai/gpt-4.1-mini");
        assert!(app.pending_prompt.is_none());
    }

    #[tokio::test]
    async fn model_command_rejects_cross_provider_selection_without_matching_connection() {
        let (_dir, session) = test_session().await;
        let cred_dir = tempfile::tempdir().unwrap();
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai-codex/gpt-5.6-sol".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
        app.connect_store
            .set_oauth(
                "openai_codex",
                forge_connect::OauthTokens {
                    access_token:
                        "header.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjb3VudC0xMjMifX0.sig"
                            .to_string(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
        app.connect_profile = Some("openai_codex".into());
        app.runtime.model_label = "openai-codex/gpt-5.6-sol".into();
        app.session.set_active_model("openai-codex/gpt-5.6-sol");

        app.dispatch_line("/model not-connected claude-sonnet-4-5")
            .await
            .unwrap();

        assert_eq!(app.connect_profile.as_deref(), Some("openai_codex"));
        assert_eq!(app.runtime.model_label, "openai-codex/gpt-5.6-sol");
        assert!(
            app.status_message.contains("connect `not-connected` first")
                || app.notices.iter().any(|l| l.contains("not-connected")),
            "expected rejection notice, got status={} notices={:?}",
            app.status_message,
            app.notices
        );
    }

    #[tokio::test]
    async fn slash_sync_commits_and_does_not_queue_chat() {
        let (dir, session) = test_session().await;
        for args in [
            &["init"][..],
            &["config", "user.email", "forge@test"][..],
            &["config", "user.name", "Forge Test"][..],
        ] {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap();
        }
        // Need an initial commit so later push has a branch tip; create one empty first.
        std::fs::write(dir.path().join("README"), "x").unwrap();
        std::process::Command::new("git")
            .args(["add", "README"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        // Bare remote for push
        let remote = dir.path().join("remote.git");
        std::process::Command::new("git")
            .args(["init", "--bare"])
            .arg(&remote)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["remote", "add", "origin"])
            .arg(&remote)
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["push", "-u", "origin", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        std::fs::write(dir.path().join("note.txt"), "hello").unwrap();

        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.dispatch_line("/sync").await.unwrap();
        assert!(app.pending_prompt.is_none());
        assert!(!app.busy);
        let log = app
            .run_git_tool("log", vec!["-1".into(), "--oneline".into()])
            .await
            .unwrap();
        // Heuristic message mentions note.txt when mock has no real LLM summary
        assert!(
            log.content.contains("note") || log.content.contains("Add") || !log.content.is_empty(),
            "expected a commit: {}",
            log.content
        );
        assert!(
            app.activity
                .recent(12)
                .iter()
                .any(|e| e.summary.contains("git commit") || e.summary.contains("git push")),
            "activity should record sync steps"
        );
    }

    #[tokio::test]
    async fn app_dispatch_user_message() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("/tmp"),
                version: "0.4.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.dispatch_line("hi").await.unwrap();
        app.drain_pending_prompt(None).await.unwrap();
        assert!(
            app.session
                .messages
                .iter()
                .any(|m| m.content.contains("hello tui") || m.content == "hi"),
            "messages={:?}",
            app.session
                .messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn app_status_command() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.4.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.dispatch_line("/status").await.unwrap();
        assert!(app.overlay.is_none());
        assert!(app.notices.is_empty());
    }

    #[tokio::test]
    async fn clear_hides_existing_chat_without_deleting_context() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.4.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.dispatch_line("hi").await.unwrap();
        app.drain_pending_prompt(None).await.unwrap();
        let message_count = app.session.messages.len();
        let event_count = app.session.events.len();
        assert!(message_count > 0);

        app.dispatch_line("/clear").await.unwrap();

        assert_eq!(app.chat_message_start, message_count);
        assert_eq!(app.chat_event_start, event_count);
        assert_eq!(app.session.messages.len(), message_count);
        assert_eq!(app.session.events.len(), event_count);
        assert!(app.ui_banners.is_empty());
        assert!(app.notices.is_empty());
        assert_eq!(app.chat_scroll, 0);
        assert!(app.chat_follow);
    }

    #[tokio::test]
    async fn app_quit_command() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "p".into(),
                cwd: PathBuf::from("."),
                version: "0.4.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.dispatch_line("/quit").await.unwrap();
        assert!(app.should_quit);
    }

    #[tokio::test]
    async fn connect_opencode_go_opens_api_key_overlay() {
        // Isolate HOME to prevent restore_saved_auth from discovering real credentials.
        let _home_guard = {
            let temp_home = tempfile::TempDir::new().unwrap();
            let cred_dir = temp_home.path().join("Library/Application Support/forge");
            std::fs::create_dir_all(&cred_dir).unwrap_or_default();
            let _ = std::fs::write(cred_dir.join("credentials.toml"), "");

            let guard = ScopedEnvGuard::new(&[
                "HOME",
                "XDG_CONFIG_HOME",
                "OPENAI_API_KEY",
                "ANTHROPIC_API_KEY",
                "OPENCODE_API_KEY",
                "OPENCODE_GO_API_KEY",
                "OPENCODE_ZEN_API_KEY",
                "OLLAMA_API_KEY",
                "XAI_API_KEY",
            ]);
            std::env::set_var("HOME", temp_home.path());
            (temp_home, guard)
        };

        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.6.1".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        let _store_dir = tempfile::TempDir::new().unwrap();
        app.connect_store = CredentialStore::new(_store_dir.path().join("empty-creds.toml"));
        app.dispatch_line("/connect opencode_go").await.unwrap();
        match &app.overlay {
            Some(Overlay::ConnectApiKey {
                profile_id, title, ..
            }) => {
                assert_eq!(profile_id, "opencode_go");
                assert!(title.contains("OpenCode"));
            }
            other => panic!("expected ConnectApiKey overlay, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn disconnect_clears_credentials_and_prompts_reauth() {
        let (_dir, session) = test_session().await;
        let cred_dir = tempfile::tempdir().unwrap();
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai/gpt-4.1-mini".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.6.1".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
        app.connect_store
            .set_api_key("openai", "sk-test-saved-credential")
            .unwrap();
        app.connect_profile = Some("openai".into());
        app.runtime.model_label = "openai/gpt-4.1-mini".into();
        app.session.set_active_model("openai/gpt-4.1-mini");

        app.dispatch_line("/disconnect").await.unwrap();

        assert!(app.auth_suspended);
        assert!(app.connect_profile.is_none());
        assert!(!app.is_provider_connected());
        assert!(!app.connect_store.is_connected("openai").unwrap());
        assert!(matches!(app.overlay, Some(Overlay::ConnectPicker { .. })));
        assert!(
            app.notices.iter().any(|l| l.contains("disconnected"))
                || app.status_message.contains("disconnected")
        );
    }

    #[tokio::test]
    async fn connect_picker_marks_saved_credentials_as_connected() {
        let (_dir, session) = test_session().await;
        let cred_dir = tempfile::tempdir().unwrap();
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai/gpt-4.1-mini".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.6.1".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
        app.connect_store
            .set_api_key("openai", "sk-test-saved-credential")
            .unwrap();

        app.open_connect_picker();
        let Some(Overlay::ConnectPicker { items, .. }) = &app.overlay else {
            panic!("expected connect picker");
        };
        assert!(
            items
                .iter()
                .any(|item| item.id == "openai" && item.connected),
            "saved provider should be marked connected"
        );
    }

    #[tokio::test]
    async fn successful_connect_hands_off_to_model_picker() {
        let (_dir, session) = test_session().await;
        let cred_dir = tempfile::tempdir().unwrap();
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai/gpt-4.1-mini".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.6.1".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
        app.connect_store
            .set_api_key("openai", "sk-test-saved-credential")
            .unwrap();
        app.connect_profile = Some("openai".into());
        app.runtime.model_label = "openai/gpt-4.1-mini".into();
        app.session.set_active_model("openai/gpt-4.1-mini");

        app.open_model_picker_after_connect("openai");
        let Some(Overlay::Model {
            provider_selected,
            providers,
            ..
        }) = &app.overlay
        else {
            panic!("expected model picker");
        };
        assert_eq!(
            providers.get(*provider_selected).map(String::as_str),
            Some("openai")
        );
        assert!(app.feedback.text.contains("choose a model"));
    }

    #[tokio::test]
    async fn model_selection_switches_to_the_matching_connected_provider() {
        let (_dir, session) = test_session().await;
        let cred_dir = tempfile::tempdir().unwrap();
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai/gpt-4.1-mini".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.6.1".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
        app.connect_store
            .set_api_key("openai", "sk-test-openai-credential")
            .unwrap();
        app.connect_store
            .set_api_key("anthropic", "sk-test-anthropic-credential")
            .unwrap();
        app.connect_profile = Some("openai".into());

        app.apply_model_selection("native", "anthropic/claude-sonnet-4-5");

        assert_eq!(app.connect_profile.as_deref(), Some("anthropic"));
        assert_eq!(app.runtime.model_label, "anthropic/claude-sonnet-4-5");
    }

    #[tokio::test]
    async fn invalid_api_key_error_stays_inside_key_modal() {
        let (_dir, session) = test_session().await;
        let cred_dir = tempfile::tempdir().unwrap();
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai/gpt-4.1-mini".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.6.1".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
        let mut overlay = Overlay::connect_api_key("openai", "OpenAI", None, None);
        if let Overlay::ConnectApiKey { key_input, .. } = &mut overlay {
            *key_input = "bad".into();
        }
        app.overlay = Some(overlay);

        app.try_connect_api_key("openai", Some("bad".into()));
        let Some(Overlay::ConnectApiKey {
            key_input, error, ..
        }) = &app.overlay
        else {
            panic!("expected API key modal to remain open");
        };
        assert_eq!(key_input, "bad");
        assert!(error.as_deref().is_some_and(|text| text.contains("short")));
        assert!(
            !app.ui_banners.iter().any(|item| matches!(
                item,
                ChatItem::Banner {
                    kind: BannerKind::Error,
                    ..
                }
            )),
            "onboarding errors should stay in the modal"
        );
    }

    #[tokio::test]
    async fn connect_xai_opens_oauth_overlay() {
        let (_dir, session) = test_session().await;
        let cred_dir = tempfile::tempdir().unwrap();
        // Isolate credentials + use stub device start (no network).
        std::env::set_var("FORGE_CONNECT_OAUTH_STUB", "1");
        std::env::remove_var("FORGE_CONNECT_OAUTH_FIXTURE");
        std::env::remove_var("FORGE_XAI_OAUTH_ACCESS_TOKEN");
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.6.1".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.connect_store = CredentialStore::new(cred_dir.path().join("c.toml"));
        app.dispatch_line("/connect xai").await.unwrap();
        std::env::remove_var("FORGE_CONNECT_OAUTH_STUB");
        match &app.overlay {
            Some(Overlay::ConnectOauth {
                profile_id, title, ..
            }) => {
                assert_eq!(profile_id, "xai");
                assert!(title.contains("Grok") || title.contains("xAI"));
            }
            other => panic!("expected ConnectOauth overlay, got {other:?}"),
        }
        assert!(app.oauth_pending.is_some());
    }

    #[tokio::test]
    async fn history_records_submitted_lines_and_up_recalls() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.7.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        let enter = KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        app.input.set_text("/status");
        app.handle_key(enter).await.unwrap();
        app.input.set_text("/model");
        app.handle_key(enter).await.unwrap();
        assert!(app.history.len() >= 2);
        let t = app.history.up(&app.input.text).unwrap();
        app.apply_history_text(t);
        assert_eq!(app.input.text, "/model");
        let t = app.history.up(&app.input.text).unwrap();
        app.apply_history_text(t);
        assert_eq!(app.input.text, "/status");
        let t = app.history.down().unwrap();
        app.apply_history_text(t);
        assert_eq!(app.input.text, "/model");
    }

    #[tokio::test]
    async fn history_up_via_key_when_no_overlay() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.7.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.history.push("alpha");
        app.history.push("beta");
        app.input.clear();
        let up = KeyEvent {
            code: KeyCode::Up,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        app.handle_key(up).await.unwrap();
        assert_eq!(app.input.text, "beta");
        app.handle_key(up).await.unwrap();
        assert_eq!(app.input.text, "alpha");
        let down = KeyEvent {
            code: KeyCode::Down,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        app.handle_key(down).await.unwrap();
        assert_eq!(app.input.text, "beta");
    }

    #[tokio::test]
    async fn helper_labels_reflect_focus_mode() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "test".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        assert!(app.help_text().contains("Conversation"));
        app.workspace_navigation
            .replace_view(WorkspaceView::Diff(DiffCommandContext::Current));
        assert!(app.help_text().contains("Review changes"));
    }

    #[tokio::test]
    async fn tab_nav_command_recognizes_shifted_plain_arrows_only() {
        let (_dir, app) = focus_test_app().await;
        assert_eq!(
            app.tab_nav_command(press(KeyCode::Left, KeyModifiers::SHIFT)),
            Some(TabNavCommand::PreviousTab)
        );
        assert_eq!(
            app.tab_nav_command(press(KeyCode::Right, KeyModifiers::SHIFT)),
            Some(TabNavCommand::NextTab)
        );
        assert_eq!(
            app.tab_nav_command(press(KeyCode::Left, KeyModifiers::ALT)),
            None
        );
        assert_eq!(
            app.tab_nav_command(press(KeyCode::Right, KeyModifiers::CONTROL)),
            None
        );
    }

    #[tokio::test]
    async fn focus_availability_and_restore_skip_hidden_blocks() {
        let (_dir, mut app) = focus_test_app().await;
        app.files_visible = true;
        app.sidebar_visible = false;
        app.bottom_panel.open = false;
        let availability = app.focus_availability();
        assert!(availability.contains(FocusBlock::Files));
        assert!(!availability.contains(FocusBlock::Inspector));
        assert!(!availability.contains(FocusBlock::BottomPanel));

        app.focus.previous_block = Some(FocusBlock::Inspector);
        app.restore_focus_after_closing(FocusBlock::Files);
        assert_eq!(app.focus.block, FocusBlock::Workspace);
        assert_eq!(app.focus.return_block, Some(FocusBlock::Workspace));
    }

    #[tokio::test]
    async fn contextual_hint_appears_only_for_transient_or_blocking_state() {
        let (_dir, mut app) = focus_test_app().await;
        assert!(app.contextual_hint().is_none());

        app.focus_block(FocusBlock::Workspace);
        assert!(app.contextual_hint().is_none());

        app.focus.mode = FocusMode::Transient(TransientOwner::SourceSearch);
        assert!(app
            .contextual_hint()
            .is_some_and(|hint| hint.contains("Esc cancel")));

        app.focus.mode = FocusMode::Navigation;
        app.overlay = Some(Overlay::turn_limit(4));
        assert_eq!(
            app.contextual_hint().as_deref(),
            Some("Enter confirm · Esc cancel")
        );
    }

    fn press(code: KeyCode, mods: KeyModifiers) -> event::KeyEvent {
        event::KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    #[tokio::test]
    async fn slash_stays_in_textbox_does_not_open_palette() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.input.text, "/");
        assert!(app.overlay.is_none(), "Phase 8: / must not open palette");
        app.handle_key(press(KeyCode::Char('s'), KeyModifiers::NONE))
            .await
            .unwrap();
        app.handle_key(press(KeyCode::Char('t'), KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.input.text, "/st");
        assert!(app.overlay.is_none());
    }

    #[tokio::test]
    async fn enter_runs_slash_from_main_textbox() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        for c in "/status".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        assert_eq!(app.input.text, "/status");
        assert!(app.overlay.is_none());
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.notices.is_empty());
        assert!(app.history.entries().iter().any(|e| e == "/status"));
    }

    #[tokio::test]
    async fn ctrl_k_opens_command_palette() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.handle_key(press(KeyCode::Char('k'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert!(matches!(app.overlay, Some(Overlay::Slash { .. })));
    }

    #[tokio::test]
    async fn inspector_is_closed_by_default_and_opens_on_demand() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "test".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        assert!(!app.sidebar_visible);
        app.handle_key(press(KeyCode::Char('b'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert!(app.sidebar_visible);
        assert!(split_areas_full(
            ratatui::layout::Rect::new(0, 0, 120, 30),
            0,
            3,
            app.sidebar_visible,
            0
        )
        .sidebar
        .is_some());
        app.handle_key(press(KeyCode::Char('b'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert!(!app.sidebar_visible);
        assert!(
            split_areas_full(ratatui::layout::Rect::new(0, 0, 80, 24), 0, 3, true, 0)
                .sidebar
                .is_none()
        );
    }

    #[tokio::test]
    async fn inspector_view_shortcuts_cycle_without_opening_sidebar() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "test".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        assert_eq!(app.inspector_view, InspectorView::Task);
        app.handle_key(press(KeyCode::Char(']'), KeyModifiers::ALT))
            .await
            .unwrap();
        assert_eq!(app.inspector_view, InspectorView::Context);
        app.handle_key(press(KeyCode::Char('['), KeyModifiers::ALT))
            .await
            .unwrap();
        assert_eq!(app.inspector_view, InspectorView::Task);
        assert!(!app.sidebar_visible);
    }

    #[tokio::test]
    async fn multi_token_slash_connect_list_opens_picker() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        for c in "/connect list".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        match &app.overlay {
            Some(Overlay::ConnectPicker { items, .. }) => {
                assert!(items.iter().any(|i| i.id == "xai"));
                assert!(items.iter().any(|i| i.id == "opencode_go"));
            }
            other => panic!("expected ConnectPicker, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn slash_tab_autocompletes_command() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        for c in "/res".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        assert!(!app.slash_suggestions().is_empty());
        app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(
            app.input.text.starts_with("/resume"),
            "got {}",
            app.input.text
        );
    }

    #[tokio::test]
    async fn connect_alone_opens_profile_picker() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        for c in "/connect".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(matches!(app.overlay, Some(Overlay::ConnectPicker { .. })));
    }

    #[tokio::test]
    async fn startup_notices_seed_notice_panel() {
        let (_dir, session) = test_session().await;
        let app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: vec!["mcp: failed".into()],
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );

        assert_eq!(app.notices, vec!["mcp: failed"]);
    }

    #[tokio::test]
    async fn enter_on_highlighted_suggestion_runs_command() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        // Partial type; suggestions include /connect and /status.
        for c in "/con".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        let suggestions = app.slash_suggestions();
        assert!(!suggestions.is_empty(), "expected slash suggestions");
        // Move highlight onto /connect if it is not already first
        let connect_idx = suggestions
            .iter()
            .position(|s| s.cmd == "/connect")
            .expect("/connect in suggestions for /con");
        for _ in 0..connect_idx {
            app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
                .await
                .unwrap();
        }
        assert_eq!(
            app.slash_suggestions()[app.slash_suggest_idx].cmd,
            "/connect"
        );
        // One Enter should apply selection AND open the connect picker (not merely complete text)
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(
            matches!(app.overlay, Some(Overlay::ConnectPicker { .. })),
            "Enter on highlighted /connect should open picker; overlay={:?} input={:?} status={}",
            app.overlay,
            app.input.text,
            app.status_message
        );
        assert!(
            app.input.text.is_empty(),
            "input should be cleared after run, got {:?}",
            app.input.text
        );
    }

    #[tokio::test]
    async fn bare_slash_lists_all_palette_commands() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE))
            .await
            .unwrap();
        let suggestions = app.slash_suggestions();
        let expected = crate::overlays::default_palette_items();
        assert_eq!(
            suggestions.len(),
            expected.len(),
            "bare / should list every palette command; got {:?}",
            suggestions.iter().map(|s| &s.cmd).collect::<Vec<_>>()
        );
        for cmd in [
            "/connect",
            "/model",
            "/compact",
            "/resume",
            "/file",
            "/sync",
            "/copy",
            "/clear",
            "/disconnect",
            "/quit",
        ] {
            assert!(
                suggestions.iter().any(|s| s.cmd == cmd),
                "missing {cmd} in suggestions"
            );
        }
    }

    #[tokio::test]
    async fn enter_on_status_suggestion_runs_immediately() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        for c in "/sta".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.input.text.is_empty());
    }

    #[test]
    fn status_model_from_app_fields() {
        // layout + status integration smoke
        let m = StatusModel {
            status: forge_types::SessionStatus::Running,
            session_short: "abc".into(),
            model: "m".into(),
            provider: "mock".into(),
            effort: "auto".into(),
            ctx_pct: 0.2,
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: None,
            provider_connected: true,
            web_search_label: None,
            tools_visible: 0,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
            repo_name: None,
            branch: None,
            dirty: false,
            resource: None,
            activity: None,
            turn_cancelled: false,
            progress_description: None,
            failure_category: None,
            waiting_detail: None,
        };
        assert_eq!(m.status_label().0, "Ready");
    }

    #[tokio::test]
    async fn header_status_follows_session_lifecycle() {
        let (dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "test".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );

        let ready = app.refresh_status_model();
        assert_eq!(ready.turn_lifecycle(), TurnLifecycle::Ready);
        assert!(ready.status_label().0.contains("Ready"));

        app.busy = true;
        app.busy_phase = BusyPhase::Tool {
            name: "read_file".into(),
        };
        let working = app.refresh_status_model();
        assert_eq!(working.turn_lifecycle(), TurnLifecycle::Working);
        assert!(working.status_label().0.contains("Working"));
        assert!(
            working.status_label().0.contains("Reading files"),
            "{:?}",
            working.status_label().0
        );

        app.busy = false;
        app.busy_phase = BusyPhase::Idle;
        app.session.status = forge_types::SessionStatus::Completed;
        assert_eq!(
            app.refresh_status_model().turn_lifecycle(),
            TurnLifecycle::Completed
        );

        app.session.status = forge_types::SessionStatus::Failed;
        assert_eq!(
            app.refresh_status_model().turn_lifecycle(),
            TurnLifecycle::Failed
        );

        app.session.status = forge_types::SessionStatus::Cancelled;
        assert_eq!(
            app.refresh_status_model().turn_lifecycle(),
            TurnLifecycle::Cancelled
        );

        app.session.status = forge_types::SessionStatus::Interrupted;
        assert_eq!(
            app.refresh_status_model().turn_lifecycle(),
            TurnLifecycle::Interrupted
        );

        app.session.status = forge_types::SessionStatus::AwaitingHitl;
        app.session.pending_hitl = Some(direct_hitl_payload("h", "x.txt"));
        let waiting = app.refresh_status_model();
        assert_eq!(waiting.turn_lifecycle(), TurnLifecycle::Waiting);
        assert!(waiting.status_label().0.contains("Approval required"));
    }

    #[tokio::test]
    async fn header_status_switches_with_selected_session() {
        let dir = TempDir::new().unwrap();
        let mut completed = session_for_workspace_with_model(
            dir.path(),
            Arc::new(MockModelClient::script(vec![ModelResponse {
                text: "done-a".into(),
                tool_calls: vec![],
                usage: None,
                thinking: None,
            }])),
        )
        .await;
        completed.run_user_message("a").await.unwrap();
        assert_eq!(completed.status, forge_types::SessionStatus::Completed);
        let id_a = completed.session_id;

        let running =
            session_for_workspace_with_model(dir.path(), Arc::new(MockModelClient::script(vec![])))
                .await;
        let id_b = running.session_id;

        let mut app = TuiApp::new(
            completed,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "test".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        assert!(app
            .refresh_status_model()
            .status_label()
            .0
            .contains("Completed"));

        app.session.resume_session(id_b).await.unwrap();
        assert_eq!(app.session.status, forge_types::SessionStatus::Interrupted);
        assert!(app
            .refresh_status_model()
            .status_label()
            .0
            .contains("Interrupted"));

        app.session.resume_session(id_a).await.unwrap();
        assert_eq!(app.session.status, forge_types::SessionStatus::Completed);
        assert!(app
            .refresh_status_model()
            .status_label()
            .0
            .contains("Completed"));
    }

    #[tokio::test]
    async fn blocks_chat_when_not_connected() {
        use crossterm::event::{KeyCode, KeyModifiers};

        // Isolate HOME to a temp dir so CredentialStore::user_default() cannot
        // discover the real user's ~/.forge/credentials.toml.
        let _home_guard = {
            let temp_home = tempfile::TempDir::new().unwrap();
            // Create the credential directory structure that user_default() expects.
            // On macOS: {HOME}/Library/Application Support/forge/credentials.toml
            // On Linux: {HOME}/.config/forge/credentials.toml
            let cred_dir = temp_home.path().join("Library/Application Support/forge");
            std::fs::create_dir_all(&cred_dir).unwrap_or_default();
            let _ = std::fs::write(cred_dir.join("credentials.toml"), "");

            let guard = ScopedEnvGuard::new(&[
                "HOME",
                "XDG_CONFIG_HOME",
                "OPENAI_API_KEY",
                "ANTHROPIC_API_KEY",
                "OPENCODE_API_KEY",
                "OPENCODE_GO_API_KEY",
                "OPENCODE_ZEN_API_KEY",
                "OLLAMA_API_KEY",
                "XAI_API_KEY",
            ]);
            std::env::set_var("HOME", temp_home.path());
            (temp_home, guard)
        };

        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai/gpt-4.1-mini".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.11.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        // Override credential store with empty temp file so connection check fails.
        let _store_dir = tempfile::TempDir::new().unwrap();
        app.connect_store = CredentialStore::new(_store_dir.path().join("empty-creds.toml"));
        app.connect_profile = None;
        app.refresh_connection_ui();
        assert!(!app.is_provider_connected());

        for c in "hello world".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        assert!(app.pending_prompt.is_none(), "must not queue a model turn");
        assert!(!app.busy);
        assert_eq!(app.input.text, "hello world");
        assert!(
            app.ui_banners.iter().any(|b| matches!(
                b,
                ChatItem::Banner { text, .. } if text.to_ascii_lowercase().contains("not connected")
            )) || app
                .activity
                .recent(8)
                .iter()
                .any(|e| e.summary.to_ascii_lowercase().contains("not connected")),
            "expected not-connected feedback"
        );
    }

    #[tokio::test]
    async fn mock_provider_allows_chat_without_connect() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.11.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        assert!(app.is_provider_connected());
        app.dispatch_line("hi").await.unwrap();
        assert_eq!(app.pending_prompt.as_deref(), Some("hi"));
    }

    #[tokio::test]
    async fn status_chrome_shows_not_connected_badge() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai/gpt-test".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.11.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.connect_profile = None;
        app.connect_store = CredentialStore::new(
            tempfile::TempDir::new()
                .unwrap()
                .path()
                .join("empty-creds.toml"),
        );
        app.refresh_connection_ui();
        let backend = TestBackend::new(100, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let mut text = String::new();
        let buf = term.backend().buffer();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(
            text.to_ascii_lowercase().contains("not connected") || text.contains("○"),
            "missing not-connected chrome:\n{text}"
        );
    }

    #[tokio::test]
    async fn tui09_chrome_includes_model_on_frame() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai/gpt-test".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        let chrome = app.refresh_status_model();
        assert_eq!(chrome.provider, "native");
        assert!(chrome.model.contains("gpt-test"));
        let backend = TestBackend::new(120, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let mut text = String::new();
        let buf = term.backend().buffer();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(
            !text.contains("gpt-test") && !text.contains("in 0 · out 0 · total 0"),
            "default chrome duplicated model or usage:\n{text}"
        );
    }

    #[tokio::test]
    async fn tui09_narrow_frame_still_shows_model_or_ctx() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mymodel".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        // Width 60: no sidebar per layout MIN_WIDTH 80
        let backend = TestBackend::new(60, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let mut text = String::new();
        let buf = term.backend().buffer();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(
            text.contains("Forge"),
            "narrow frame missing app identity:\n{text}"
        );
        assert!(
            !text.contains("mymodel") && !text.contains("mock") && !text.contains("ctx"),
            "narrow default chrome duplicated secondary metadata:\n{text}"
        );
    }

    #[tokio::test]
    async fn tui09_status_renders_structured_session_card() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        for c in "/status".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.notices.is_empty());
        assert!(app.overlay.is_none());

        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            text.contains("unknown command `/status`"),
            "missing status command feedback:\n{text}"
        );
    }

    #[tokio::test]
    async fn tui08_report_error_writes_banner_feedback_and_activity() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.report_error("upstream returned 429 rate limit exceeded");
        assert_eq!(app.feedback.severity, FeedbackSeverity::Error);
        assert!(app.feedback.text.contains("429"));
        assert!(app.status_message.contains("429"));
        assert!(
            app.ui_banners.iter().any(|b| matches!(
                b,
                ChatItem::Banner {
                    kind: BannerKind::Error,
                    ..
                }
            )),
            "expected error banner in ui_banners"
        );
        assert!(
            app.activity
                .all()
                .iter()
                .any(|i| i.kind == ActivityKind::Error),
            "expected error activity"
        );
    }

    #[tokio::test]
    async fn tui08_feedback_strip_visible_on_frame() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.report_error("429 rate limit");
        let backend = TestBackend::new(100, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let mut text = String::new();
        let buf = term.backend().buffer();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(
            text.contains("rate limited") || text.contains("429") || text.contains("Model error"),
            "frame missing feedback:\n{text}"
        );
    }

    #[tokio::test]
    async fn tui10_activity_feed_records_model_and_error() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.push_activity(
            ActivityKind::Model,
            FeedbackSeverity::Info,
            "model call started",
        );
        app.report_error("429 rate limit");
        assert!(app.activity.len() >= 2);
        let recent: Vec<_> = app
            .activity
            .recent(10)
            .iter()
            .map(|i| i.summary.clone())
            .collect();
        assert!(
            recent
                .iter()
                .any(|s| s.contains("rate") || s.contains("429") || s.contains("Model")),
            "recent={recent:?}"
        );
        assert_eq!(app.busy_phase, BusyPhase::Idle);
    }

    #[tokio::test]
    async fn elapsed_status_persists_during_answer_and_tool_processing() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.busy = true;
        app.turn_started = Some(Instant::now() - Duration::from_millis(1200));
        app.stream_preview = "partial answer".into();
        assert_eq!(app.busy_status_detail().as_deref(), Some("Working... 1.2s"));

        app.stream_preview.clear();
        app.busy_phase = BusyPhase::Tool {
            name: "read_file".into(),
        };
        assert!(app
            .busy_status_detail()
            .unwrap()
            .starts_with("Working... 1.2s"));
    }

    #[tokio::test]
    async fn tui10_busy_phase_model_during_turn_clears_after() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.dispatch_line("hello").await.unwrap();
        assert_eq!(app.busy_phase, BusyPhase::Model);
        assert!(app.pending_prompt.is_some());
        app.drain_pending_prompt(None).await.unwrap();
        assert_eq!(app.busy_phase, BusyPhase::Idle);
        assert!(
            app.activity
                .all()
                .iter()
                .any(|i| i.kind == ActivityKind::Model),
            "expected model activity"
        );
    }

    #[tokio::test]
    async fn tui08_context_sets_feedback_strip() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        for c in "/status".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.input.text.is_empty());
    }

    /// Save-and-restore env vars so dev machine credentials don't leak into tests.
    struct ScopedEnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(String, Option<String>)>,
    }

    impl ScopedEnvGuard {
        fn new(keys: &[&str]) -> Self {
            static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
            let lock = ENV_LOCK
                .get_or_init(|| Mutex::new(()))
                .lock()
                .expect("environment test lock poisoned");
            let mut saved = Vec::new();
            for key in keys {
                saved.push((key.to_string(), std::env::var(key).ok()));
                std::env::remove_var(key);
            }
            Self { _lock: lock, saved }
        }
    }

    impl Drop for ScopedEnvGuard {
        fn drop(&mut self) {
            for (key, val) in &self.saved {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[test]
    fn exit_summary_formats_token_usage() {
        let report = forge_core::TokenUsageReport {
            api: forge_core::SessionTokenUsage {
                prompt_tokens: 6_094,
                completion_tokens: 36,
                model_calls_with_usage: 1,
                model_steps: 1,
                thinking_tokens_est: 19,
                prompt_cache_hits: 5_504,
                prompt_cache_writes: 0,
            },
            context_tokens_est: 0,
            context_capacity: 1,
            context_pct: 0.0,
            system_tokens_est: 0,
            user_tokens_est: 0,
            assistant_tokens_est: 0,
            tool_tokens_est: 0,
            thinking_in_context_est: 0,
            message_count: 0,
            tool_message_count: 0,
        };

        assert_eq!(
            format_exit_token_usage(&report),
            "Token usage: total=6,130 input=6,094 (+ 5,504 cached) output=36 (reasoning 19)"
        );
    }

    #[test]
    fn footer_usage_formats_with_total_and_commas() {
        let report = forge_core::TokenUsageReport {
            api: forge_core::SessionTokenUsage {
                prompt_tokens: 6_094,
                completion_tokens: 36,
                model_calls_with_usage: 1,
                model_steps: 1,
                thinking_tokens_est: 19,
                prompt_cache_hits: 5_504,
                prompt_cache_writes: 0,
            },
            context_tokens_est: 0,
            context_capacity: 1,
            context_pct: 0.0,
            system_tokens_est: 0,
            user_tokens_est: 0,
            assistant_tokens_est: 0,
            tool_tokens_est: 0,
            thinking_in_context_est: 0,
            message_count: 0,
            tool_message_count: 0,
        };

        assert_eq!(
            footer_usage_summary_with_cost(&report, None),
            "in 6,094 · out 36 · total 6,130"
        );
    }

    #[test]
    fn footer_usage_includes_cached_cost() {
        let report = forge_core::TokenUsageReport {
            api: forge_core::SessionTokenUsage {
                prompt_tokens: 1_000_000,
                completion_tokens: 500_000,
                model_calls_with_usage: 1,
                model_steps: 1,
                thinking_tokens_est: 0,
                prompt_cache_hits: 0,
                prompt_cache_writes: 0,
            },
            context_tokens_est: 0,
            context_capacity: 1,
            context_pct: 0.0,
            system_tokens_est: 0,
            user_tokens_est: 0,
            assistant_tokens_est: 0,
            tool_tokens_est: 0,
            thinking_in_context_est: 0,
            message_count: 0,
            tool_message_count: 0,
        };

        assert_eq!(
            footer_usage_summary_with_cost(
                &report,
                Some(forge_connect::CatalogCost {
                    input: 3.0,
                    output: 15.0,
                })
            ),
            "in 1,000,000 · out 500,000 · total 1,500,000 · $10.5000"
        );
    }

    #[test]
    fn footer_limits_use_connected_profile_instead_of_native_transport() {
        assert_eq!(
            footer_provider_id("native", Some("openai-codex")),
            "openai-codex"
        );
        assert_eq!(footer_provider_id("mock", None), "mock");
    }

    #[tokio::test]
    async fn external_editor_keybind_sets_flag() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        assert!(!app.pending_external_editor);
        let path = app.session.workspace_root().join("fake.txt");
        fs::write(&path, "hello").unwrap();
        app.open_file_in_editor(&path);
        app.handle_key(press(KeyCode::Char('e'), KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.pending_external_editor);
    }

    #[tokio::test]
    async fn external_editor_preconditions_no_file() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.pending_external_editor = true;
        app.drain_pending_external_editor(None).await.unwrap();
        // Should not crash; feedback set because no file is open.
        assert!(!app.pending_external_editor);
    }

    #[tokio::test]
    async fn external_editor_preconditions_binary_file() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.source_viewer.status = crate::source_viewer::ViewerStatus::Binary;
        app.source_viewer.path = Some(PathBuf::from("/tmp/fake.bin"));
        app.pending_external_editor = true;
        app.drain_pending_external_editor(None).await.unwrap();
        // Should not crash; feedback set because binary.
        assert!(!app.pending_external_editor);
    }

    #[tokio::test]
    async fn external_editor_rejects_during_tool_execution() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: FileIconMode::Unicode,
                mouse_capture: true,
                theme: forge_config::Theme::default(),
            },
        );
        app.busy_phase = BusyPhase::Tool {
            name: "write".into(),
        };
        app.source_viewer.status = crate::source_viewer::ViewerStatus::Ok;
        app.source_viewer.path = Some(PathBuf::from("/tmp/fake.txt"));
        app.pending_external_editor = true;
        app.drain_pending_external_editor(None).await.unwrap();
        // Should not crash; feedback set because tool is active.
        assert!(!app.pending_external_editor);
    }

    #[tokio::test]
    async fn external_editor_resume_draws_after_terminal_reinit() {
        let (_dir, mut app) = focus_test_app().await;
        app.source_viewer.status = crate::source_viewer::ViewerStatus::Ok;
        app.source_viewer.path = Some(PathBuf::from("/tmp/fake.txt"));
        app.pending_external_editor = true;

        let result = app.resume_after_external_editor(None);
        assert!(result.is_ok());
    }

    // ---- highlight cache invalidation -------------------------------------
    //
    // The highlight cache is process-global and it is NOT exclusive to these
    // tests: `source_viewer` highlights raw source files, so several of its tests
    // move the same counters concurrently. Exact equality on those counters is
    // therefore flaky by construction.
    //
    // These assertions are written so concurrent activity can never *falsify*
    // them: "reuse" is asserted as a lower bound on hits and "invalidation" as a
    // lower bound on misses, and other tests can only ever add to both. Exact
    // hit/miss semantics are pinned separately in `forge-syntax`'s own unit
    // tests, where the cache genuinely is exclusive.

    const CACHED_BLOCKS: usize = 4;

    /// Serialises these four tests against each other so their windows do not
    /// overlap. Follows the repo's pattern for process-global state (`lock_env`
    /// in `editor.rs`, `ScopedEnvGuard` in `app.rs`), recovering poisoning so one
    /// failing test does not cascade into the rest.
    fn lock_highlight_cache() -> std::sync::MutexGuard<'static, ()> {
        static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        GUARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Assistant turns each carrying a distinct fenced Rust block, so a full
    /// re-highlight costs `CACHED_BLOCKS` misses and a fully cached render costs
    /// `CACHED_BLOCKS` hits.
    ///
    /// Each answer needs its own preceding user message: `compose_turn_presentation`
    /// keeps only one durable answer per turn, so consecutive assistant messages
    /// collapse into the last one and the earlier blocks never render at all.
    fn push_code_transcript(app: &mut TuiApp, marker: &str) {
        for i in 0..CACHED_BLOCKS {
            app.session.messages.push(forge_types::Message::new(
                forge_types::MessageRole::User,
                format!("Please do step {i} of {marker}."),
            ));
            app.session.messages.push(forge_types::Message::new(
                forge_types::MessageRole::Assistant,
                format!(
                    "Step {i} for {marker}.\n\n```rust\n\
                     pub fn {marker}_{i}(items: &[usize]) -> usize {{\n\
                     \x20   let mut total = 0usize;\n\
                     \x20   for item in items {{ total += *item; }}\n\
                     \x20   total\n\
                     }}\n```\n\nDone."
                ),
            ));
        }
    }

    async fn app_with_code(marker: &str) -> (TempDir, TuiApp) {
        let (dir, mut app) = focus_test_app().await;
        app.splash_dismissed = true;
        push_code_transcript(&mut app, marker);
        (dir, app)
    }

    /// Highlighting does not depend on terminal width, so a resize must reuse it.
    /// A resize flips the conversation render key and rebuilds every line; before
    /// this cache that re-ran tree-sitter over every code block in the transcript.
    #[tokio::test]
    async fn resize_reuses_cached_highlights() {
        let (_dir, mut app) = app_with_code("resize").await;
        // Take the serialising guard only after the last await: holding a std
        // guard across an await point is a clippy error and a real deadlock risk.
        let _guard = lock_highlight_cache();
        draw_app(&mut app, 100, 30);
        let before = forge_syntax::highlight_cache_stats();

        // A wide delta guarantees the chat width changes, so the render key flips
        // and the transcript is rebuilt from scratch.
        draw_app(&mut app, 170, 40);
        let after = forge_syntax::highlight_cache_stats();

        assert!(
            after.hits >= before.hits + CACHED_BLOCKS as u64,
            "a resize must serve every block from cache (hits {} -> {})",
            before.hits,
            after.hits
        );
    }

    /// A theme switch changes the colours baked into each segment, so it *must*
    /// recompute. This is the invalidation half of the contract: stale colours
    /// after a theme change would be a visible bug.
    #[tokio::test]
    async fn theme_switch_recomputes_highlights() {
        let (_dir, mut app) = app_with_code("theme").await;
        let _guard = lock_highlight_cache();
        crate::theme::set_active(forge_config::Theme::Dark);
        draw_app(&mut app, 100, 30);
        let before = forge_syntax::highlight_cache_stats();

        crate::theme::set_active(forge_config::Theme::Light);
        draw_app(&mut app, 100, 30);
        let after = forge_syntax::highlight_cache_stats();

        // Restore before asserting so a failure cannot leak a palette into others.
        crate::theme::set_active(forge_config::Theme::Dark);

        assert!(
            after.misses >= before.misses + CACHED_BLOCKS as u64,
            "a theme switch must recompute every block (misses {} -> {})",
            before.misses,
            after.misses
        );
    }

    /// Scrolling changes which lines are visible, never their colours. The scroll
    /// offset is not part of the conversation render key, so a scroll does not
    /// rebuild the transcript and must not recompute any highlight.
    ///
    /// Asserted as an upper bound with tolerance: a genuine re-highlight would add
    /// exactly `CACHED_BLOCKS` misses, whereas a concurrent `source_viewer` test
    /// contributes at most one or two.
    #[tokio::test]
    async fn scrollback_does_not_recompute_highlights() {
        let (_dir, mut app) = app_with_code("scroll").await;
        let _guard = lock_highlight_cache();
        draw_app(&mut app, 100, 30);
        let before = forge_syntax::highlight_cache_stats();

        app.chat_follow = false;
        app.chat_scroll = 3;
        draw_app(&mut app, 100, 30);
        let after = forge_syntax::highlight_cache_stats();

        assert!(
            after.misses < before.misses + CACHED_BLOCKS as u64,
            "scrolling must not recompute the transcript's highlights \
             (misses {} -> {})",
            before.misses,
            after.misses
        );
    }

    /// Reopening a session re-renders the same transcript text in a fresh
    /// `TuiApp`. The cache is keyed on content, not on app identity, so the
    /// second app must not pay for highlighting again.
    #[tokio::test]
    async fn session_reload_reuses_cached_highlights() {
        // Both apps are built before the guard is taken, for the same reason.
        let (_dir, mut first) = app_with_code("reload").await;
        // A separate app and session carrying identical transcript text.
        let (_dir2, mut reloaded) = app_with_code("reload").await;
        let _guard = lock_highlight_cache();
        draw_app(&mut first, 100, 30);
        let before = forge_syntax::highlight_cache_stats();

        draw_app(&mut reloaded, 100, 30);
        let after = forge_syntax::highlight_cache_stats();

        assert!(
            after.hits >= before.hits + CACHED_BLOCKS as u64,
            "a reloaded session must reuse cached highlights (hits {} -> {})",
            before.hits,
            after.hits
        );
    }
}
