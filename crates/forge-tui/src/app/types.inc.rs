// Included into `app/mod.rs` per #19 — same module scope as tests.

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
    pub(crate) session: AgentSession,
    input: InputModel,
    pub(crate) overlay: Option<Overlay>,
    should_quit: bool,
    busy: bool,
    pub(crate) status_message: String,
    pub(crate) runtime: TuiRuntimeConfig,
    last_exit: ExitCode,
    connect: connect::ConnectionModel,
    /// Phase 7 — submitted command history (Up/Down when no overlay).
    history: InputHistory,
    /// Phase 8 autocomplete: selection within filtered `/` suggestions.
    slash_suggest_idx: usize,
    /// Multi-line notices (e.g. /connect list) shown above the input.
    pub(crate) notices: Vec<String>,
    notices_until: Option<Instant>,
    /// Phase 10 / TUI-08 — always-visible feedback strip model.
    feedback: FeedbackModel,
    /// Phase 10 / TUI-08 — durable UI error/info banners in chat.
    ui_banners: Vec<ChatItem>,
    /// Phase 10 / TUI-10 — progressive busy phase for chrome.
    busy_phase: BusyPhase,
    /// User prompt queued on Enter; drained by the event loop so the YOU bubble paints first.
    pending_prompt: Option<String>,
    /// Resume the current agent loop after an interactive turn-limit checkpoint.
    pending_turn_continue: bool,
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
    web_search_label: Option<String>,
    /// Phase 10 / TUI-10 — activity ring buffer.
    activity: ActivityFeed,
    /// Reasoning effort sent to model providers (`auto` omits the parameter).
    reasoning_effort: ReasoningEffort,
    /// Expand last tool detail (Ctrl+O).
    tool_expanded: bool,
    /// V3.1 contextual workspace navigation.
    workspace_navigation: WorkspaceNavigation,
    /// Read-only source viewer state for the File workspace view.
    pub(crate) source_viewer: SourceViewer,
    file_watcher: Option<RecommendedWatcher>,
    file_change_rx: Receiver<FileChangeEvent>,
    file_change_tx: Sender<FileChangeEvent>,
    bottom_panel: BottomPanelState,
    run: RunStateModel,
    pub(crate) files_visible: bool,
    pub(crate) file_explorer: FileExplorer,
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
