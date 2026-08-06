// Included into `app/mod.rs` per #19 — same module scope as tests.

const WORKSPACE_HISTORY_LIMIT: usize = 32;
const UI_STATE_VERSION: u32 = 2;

/// Center-pane content. Conversation isn't a variant here — it's always
/// shown in the persistent sidebar instead (see [[project_ide_layout_design_round2]]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkspaceView {
    File(PathBuf),
    Diff(DiffCommandContext),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceViewKind {
    File,
    Diff,
}

impl WorkspaceView {
    fn kind(&self) -> WorkspaceViewKind {
        match self {
            Self::File(_) => WorkspaceViewKind::File,
            Self::Diff(_) => WorkspaceViewKind::Diff,
        }
    }
}

/// `current == None` means the center pane is empty (nothing open) — the
/// widget renders a placeholder in that case; see `render.rs`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct WorkspaceNavigation {
    current: Option<WorkspaceView>,
    history: Vec<WorkspaceView>,
}

impl WorkspaceNavigation {
    fn push_view(&mut self, view: WorkspaceView) {
        if self.current.as_ref() == Some(&view) {
            return;
        }
        if let Some(current) = self.current.take() {
            self.history.push(current);
            if self.history.len() > WORKSPACE_HISTORY_LIMIT {
                let overflow = self.history.len() - WORKSPACE_HISTORY_LIMIT;
                self.history.drain(0..overflow);
            }
        }
        self.current = Some(view);
    }

    fn replace_view(&mut self, view: WorkspaceView) {
        self.current = Some(view);
    }

    fn navigate_to(&mut self, view: WorkspaceView) {
        if self.current.as_ref().map(WorkspaceView::kind) == Some(view.kind()) {
            self.replace_view(view);
        } else {
            self.push_view(view);
        }
    }

    fn home(&mut self) {
        self.history.clear();
        self.current = None;
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
    #[serde(default)]
    permission_mode: Option<forge_governance::PermissionMode>,
}

#[derive(Debug, Clone)]
struct FileChangeEvent {
    path: PathBuf,
}

struct FileWatchState {
    watcher: Option<RecommendedWatcher>,
    change_rx: Receiver<FileChangeEvent>,
    change_tx: Sender<FileChangeEvent>,
}

/// The spatially stable keyboard regions.  This is intentionally small:
/// component-specific selection state remains with the component itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FocusBlock {
    Files,
    Workspace,
    Sidebar,
    Composer,
    BottomPanel,
    /// The pending human-approval card inside the transcript. Present in the
    /// focus cycle only while a HITL request is outstanding.
    Approval,
}

impl FocusBlock {
    fn label(self) -> &'static str {
        match self {
            Self::Files => "FILES",
            Self::Workspace => "CHAT",
            Self::Sidebar => "SIDEBAR",
            Self::Composer => "COMPOSER",
            Self::BottomPanel => "PANEL",
            Self::Approval => "APPROVAL",
        }
    }
}

impl FocusBlock {
    // Sidebar sits right before Composer since they're the same physical
    // column post-sidebar layout (background strip above the composer that
    // lives inside it) — tabbing out of the transcript naturally lands in
    // its own composer next. The approval card is reachable in the cycle
    // while a decision is pending.
    const ORDER: [Self; 6] = [
        Self::Files,
        Self::Workspace,
        Self::Sidebar,
        Self::Approval,
        Self::Composer,
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActivitySummaryAction {
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
    ToggleFiles,
    CloseOverlay,
    FocusComposer,
    FocusPane(FocusBlock),
    SubmitMessage,
    InsertComposerNewline,
    OpenSlashCommands,
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
    ToggleBottomPanel,
    OpenQuickOpen,
    QuickSwitchModel,
    /// Open the persistent footer control's compact picker, focused on the
    /// given column (vendor/route, model, or effort).
    OpenModelControl(ConnectModelColumn),
    OpenBottomPanel,
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
    CyclePermissionMode,
    /// Focus the composer chip bar (`F3`); ←/→ move, Enter activates.
    FocusComposerChips,
    MoveQueueSelection(i32),
    CancelSelectedQueueMessage,
    MoveTasksSelection(i32),
    CancelSelectedBackgroundTask,
    ApproveSelectedBackgroundTask,
    DenySelectedBackgroundTask,
    QuitOrInterrupt,
    Quit,
}

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
    sidebar: bool,
    bottom_panel: bool,
    approval: bool,
}

impl FocusAvailability {
    fn contains(self, block: FocusBlock) -> bool {
        match block {
            FocusBlock::Files => self.files,
            FocusBlock::Workspace => true,
            FocusBlock::Sidebar => self.sidebar,
            FocusBlock::Composer => true,
            FocusBlock::BottomPanel => self.bottom_panel,
            FocusBlock::Approval => self.approval,
        }
    }
}

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

#[derive(Debug, Clone)]
pub struct TuiRuntimeConfig {
    pub model_label: String,
    pub provider: String,
    pub cwd: PathBuf,
    pub version: String,
    pub startup_notices: Vec<String>,
    pub file_icons: FileIconMode,
    pub theme_id: String,
}

impl Default for TuiRuntimeConfig {
    fn default() -> Self {
        Self {
            model_label: String::new(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "test".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::default(),
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        }
    }
}

// ponytail: usage-summary fields are populated by the background refresh but
// no surface renders them yet; keep for the upcoming usage-summary display.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
struct FooterLimits {
    usage: String,
    weekly_limit: String,
    credits: String,
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
    status: forge_types::TaskLifecycle,
    theme_id: String,
    /// Pending HITL request identity, so the inline approval item rebuilds
    /// when a new request replaces the previous one while still `Waiting`.
    pending_hitl: Option<String>,
    approval_menu_selected: usize,
    approval_focused: bool,
    pattern_nudge: Option<(String, usize)>,
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

struct FooterLimitsState {
    cache: Option<FooterLimitsCache>,
    refresh_rx: Option<std::sync::mpsc::Receiver<(String, FooterLimits)>>,
}

struct ModelCostState {
    model: String,
    cost: Option<forge_connect::CatalogCost>,
}

struct ConversationViewState {
    message_start: usize,
    event_start: usize,
    scroll: u16,
    follow: bool,
    context_reset_snapshot: Option<(f64, f64)>,
    splash_dismissed: bool,
}

struct DiffViewState {
    selected: usize,
    snapshot: DiffSnapshot,
}

struct WorkspaceSearchState {
    index: Option<Arc<forge_search::WorkspaceIndex>>,
    error: Option<String>,
}

pub(crate) struct WorkspaceFilesState {
    pub(crate) visible: bool,
    pub(crate) explorer: FileExplorer,
}

struct TaskSelectionState {
    queue: Option<usize>,
    tasks: Option<usize>,
}

struct StartupResumeState {
    picker: bool,
    session_id: Option<uuid::Uuid>,
}

struct TurnTimingState {
    started: Option<Instant>,
    thinking_started: Option<Instant>,
    thought_secs: Option<f64>,
}

struct ExternalEditorState {
    requested: bool,
}

struct PendingTurnState {
    prompt: Option<String>,
    continue_turn: bool,
}

struct PendingInteractionState {
    hitl_decision: Option<HitlDecision>,
    context_reset: bool,
}

struct AttachmentState {
    pending: Option<crate::file_context::FileAttachment>,
}

#[derive(Default)]
struct ExplorerDialogState {
    current: Option<ExplorerDialog>,
}

struct CancellationState {
    requested: bool,
}

struct ToolDetailState {
    expanded: bool,
}

struct SearchStatusState {
    label: Option<String>,
}

struct ReasoningEffortState {
    value: ReasoningEffort,
}

struct EditorViewportState {
    height: u16,
}

struct SlashSuggestionState {
    selected: usize,
}

struct ExitState {
    requested: bool,
    code: ExitCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalMenuKind {
    AllowOnce,
    AllowPattern,
    Remember,
    Deny,
}

#[derive(Debug, Clone, Default)]
struct ApprovalMenuState {
    /// `call_id` of the pending payload this menu was built for.
    call_id: Option<String>,
    selected: usize,
}

/// After "Allow once" on a pattern-eligible call: offer to persist the pattern.
#[derive(Debug, Clone)]
struct PatternNudgeState {
    pattern: String,
    /// 0 = Yes, allow pattern · 1 = No thanks
    selected: usize,
}

struct HitlSessionState {
    allowed: HashSet<ApprovalIdentity>,
    /// Pattern rules added via "allow this pattern going forward" this
    /// session — takes effect immediately, independent of whether the
    /// write to the persisted permissions file succeeded.
    pattern_allow: Vec<forge_governance::PatternRule>,
    menu: ApprovalMenuState,
    pattern_nudge: Option<PatternNudgeState>,
}

struct ToastState {
    current: Option<(Instant, String)>,
}

pub(crate) struct NoticeState {
    pub(crate) items: Vec<String>,
    until: Option<Instant>,
}

struct BannerState {
    items: Vec<ChatItem>,
}

struct RenderCacheState {
    conversation: Option<ConversationRenderCache>,
}

struct BusyState {
    active: bool,
    phase: BusyPhase,
}

pub(crate) struct StatusMessageState {
    pub(crate) message: String,
}

struct StreamState {
    preview: String,
    thinking: String,
    live_lines: Option<(u16, usize, usize, Arc<Vec<Line<'static>>>)>,
    /// When the live preview was last re-rendered; throttles the per-token
    /// markdown rebuild so a long stream is O(n) renders, not O(tokens).
    last_preview_render: Option<Instant>,
}
pub struct TuiApp {
    pub(crate) session: AgentSession,
    input: InputModel,
    pub(crate) overlay: Option<Overlay>,
    exit: ExitState,
    startup_resume: StartupResumeState,
    busy_state: BusyState,
    pub(crate) status_state: StatusMessageState,
    pub(crate) runtime: TuiRuntimeConfig,
    connect: connect::ConnectionModel,
    /// Phase 7 — submitted command history (Up/Down when no overlay).
    history: InputHistory,
    slash_suggestions: SlashSuggestionState,
    pub(crate) notice_state: NoticeState,
    /// Phase 10 / TUI-08 — always-visible feedback strip model.
    feedback: FeedbackModel,
    feedback_until: Option<Instant>,
    banner_state: BannerState,
    /// Phase 10 / TUI-10 — progressive busy phase for chrome.
    pending_turn: PendingTurnState,
    pending_interaction: PendingInteractionState,
    /// External-editor request queued for the event loop (terminal suspend/resume).
    external_editor: ExternalEditorState,
    attachment: AttachmentState,
    /// Selected queued row for keyboard cancellation.
    task_selection: TaskSelectionState,
    /// Live assistant text while tokens stream in.
    stream: StreamState,
    timing: TurnTimingState,
    search_status: SearchStatusState,
    /// Phase 10 / TUI-10 — activity ring buffer.
    activity: ActivityFeed,
    reasoning_effort: ReasoningEffortState,
    /// Active oversight level — cycled with F2 (`SemanticCommand::CyclePermissionMode`).
    /// Mirrors what's actually applied to `session`'s `Governance` via
    /// `apply_permission_mode`; this field exists only because `Governance`
    /// doesn't remember which named mode produced its current fields.
    permission_mode: forge_governance::PermissionMode,
    /// When `Some`, composer chip bar is focused at this index (`F3`).
    composer_chip_focus: Option<usize>,
    tool_detail: ToolDetailState,
    /// V3.1 contextual workspace navigation.
    workspace_navigation: WorkspaceNavigation,
    /// Read-only source viewer state for the File workspace view.
    pub(crate) source_viewer: SourceViewer,
    file_watch: FileWatchState,
    bottom_panel: BottomPanelState,
    pub(crate) workspace_files: WorkspaceFilesState,
    explorer_dialog: ExplorerDialogState,
    /// Authoritative keyboard ownership. Legacy component `focused` flags are
    /// synchronised from this state for rendering only.
    focus: FocusState,
    /// Selected index in the changed-files inventory for Diff workspace.
    diff_view: DiffViewState,
    cancellation: CancellationState,
    hitl_session: HitlSessionState,
    toast: ToastState,
    editor_viewport: EditorViewportState,
    /// Session message/event offsets hidden by the most recent `/clear`.
    conversation_view: ConversationViewState,
    render_cache: RenderCacheState,
    model_cost_cache: Option<ModelCostState>,
    footer_limits: FooterLimitsState,
    /// Last known repo header. Refreshed off-thread by `poll_repo_header`; the
    /// render path only ever reads it, never derives it.
    repo_header_state: RepoHeaderState,
    progress_state: std::cell::RefCell<ProgressState>,
    interactive_terminal: Option<InteractiveTerminal>,
    workspace_search: WorkspaceSearchState,
    /// Editor pane's terminal rect from the most recent draw, used for mouse
    /// hit-testing (mouse events arrive between frames).
    editor_area: Option<ratatui::layout::Rect>,
    /// Composer's terminal rect from the most recent draw, used by key
    /// handling (which runs before the next render) to compute wrap width
    /// for cursor line navigation.
    pub(crate) composer_area: Option<ratatui::layout::Rect>,
    /// Active mouse text selection (v1: Editor pane).
    pub(crate) selection: crate::selection::MouseSelection,
    /// Open right-click context menu, if any.
    pub(crate) context_menu: Option<crate::selection::ContextMenu>,
    pub(crate) conversation_area: Option<ratatui::layout::Rect>,
    pub(crate) conversation_rows: Vec<String>,
    pub(crate) diff_area: Option<ratatui::layout::Rect>,
    pub(crate) diff_rows: Vec<String>,
    pub(crate) terminal_area: Option<ratatui::layout::Rect>,
    pub(crate) terminal_rows: Vec<String>,
    catalog_fetch: CatalogFetchState,
}

#[derive(Debug, Clone)]
pub(crate) struct RepoHeaderCache {
    pub(crate) repo_name: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) dirty: bool,
}

struct RepoHeaderState {
    cache: RepoHeaderCache,
    refresh_rx: Option<std::sync::mpsc::Receiver<RepoHeaderCache>>,
    refreshed_at: Instant,
    /// Directory the cached header describes, so a cwd change invalidates it.
    cwd: PathBuf,
}

#[derive(Debug, Default)]
struct ProgressState {
    modified: Option<std::time::SystemTime>,
    description: Option<String>,
}

/// Off-thread model-catalog refresh, matching `RepoHeaderState`'s
/// spawn-a-thread-and-poll shape. The worker thread refreshes
/// `ModelCatalogCache`'s on-disk file as a side effect and reports back only
/// success/failure — callers re-read the (now warm) cache via the existing
/// synchronous `model_picker_items(false)` rather than threading fetched data
/// through the channel, so there is never a second in-memory catalog that
/// could drift from the disk cache `models_for_picker` already owns.
struct CatalogFetchState {
    refresh_rx: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    /// Set once the first background refresh has ever been kicked off this
    /// session, so the lazy first-render warm-up in `draw()` (see
    /// `footer_has_compact_control`) fires at most once per app lifetime —
    /// every later refresh is triggered explicitly by opening a picker.
    warmed: bool,
}
