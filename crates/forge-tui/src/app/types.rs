//! State types owned by the TUI application.
//!
//! This is a real module so type ownership and visibility are explicit rather
//! than being textually injected into `app`.

use super::*;

pub(crate) const WORKSPACE_HISTORY_LIMIT: usize = 32;
pub(crate) const UI_STATE_VERSION: u32 = 2;

/// Center-pane content. Conversation isn't a variant here — it's always
/// shown in the persistent sidebar instead (see [[project_ide_layout_design_round2]]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceView {
    File(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceViewKind {
    File,
}

impl WorkspaceView {
    pub(crate) fn kind(&self) -> WorkspaceViewKind {
        match self {
            Self::File(_) => WorkspaceViewKind::File,
        }
    }
}

/// `current == None` means the center pane is empty (nothing open) — the
/// widget renders a placeholder in that case; see `render.rs`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WorkspaceNavigation {
    current: Option<WorkspaceView>,
    history: Vec<WorkspaceView>,
}

impl WorkspaceNavigation {
    pub(crate) fn current(&self) -> Option<WorkspaceView> {
        self.current.clone()
    }

    #[cfg(test)]
    pub(crate) fn history(&self) -> &[WorkspaceView] {
        &self.history
    }

    pub(crate) fn push_view(&mut self, view: WorkspaceView) {
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

    pub(crate) fn replace_view(&mut self, view: WorkspaceView) {
        self.current = Some(view);
    }

    pub(crate) fn navigate_to(&mut self, view: WorkspaceView) {
        if self.current.as_ref().map(WorkspaceView::kind) == Some(view.kind()) {
            self.replace_view(view);
        } else {
            self.push_view(view);
        }
    }

    pub(crate) fn home(&mut self) {
        self.history.clear();
        self.current = None;
    }

    pub(crate) fn pop_previous_valid(
        &mut self,
        is_valid: impl Fn(&WorkspaceView) -> bool,
    ) -> Option<WorkspaceView> {
        while let Some(candidate) = self.history.pop() {
            if is_valid(&candidate) {
                self.current = Some(candidate.clone());
                return Some(candidate);
            }
        }
        self.current = None;
        None
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum FilesVisibility {
    Open,
    #[default]
    Closed,
}

impl FilesVisibility {
    pub(crate) fn from_open(open: bool) -> Self {
        if open {
            Self::Open
        } else {
            Self::Closed
        }
    }

    pub(crate) fn is_open(self) -> bool {
        matches!(self, Self::Open)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RepositoryUiState {
    #[serde(default)]
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) repository_or_workspace_id: String,
    #[serde(default)]
    pub(crate) files_visibility: FilesVisibility,
    #[serde(default)]
    pub(crate) theme: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct FileChangeEvent {
    pub(super) path: PathBuf,
    /// Content-only modifications do not require rebuilding the explorer tree.
    pub(super) tree_changed: bool,
    /// Tests and explicit injections are already a complete logical event.
    pub(super) immediate: bool,
}

#[derive(Debug)]
pub(super) struct FileChangeBatch {
    pub(super) paths: Vec<PathBuf>,
    pub(super) tree_changed: bool,
}

/// Owns the watcher lifecycle and coalesces notify's many low-level events into
/// one logical workspace change. Callers cannot replace the channel state.
pub(crate) struct FileWatchState {
    watcher: Option<RecommendedWatcher>,
    change_rx: Receiver<FileChangeEvent>,
    change_tx: Sender<FileChangeEvent>,
    pending: std::collections::HashMap<PathBuf, bool>,
    ready_at: Option<Instant>,
    deferred_tree_refresh: bool,
}

impl FileWatchState {
    pub(super) const DEBOUNCE: Duration = Duration::from_millis(100);

    pub(crate) fn new() -> Self {
        let (change_tx, change_rx) = mpsc::channel();
        Self {
            watcher: None,
            change_rx,
            change_tx,
            pending: std::collections::HashMap::new(),
            ready_at: None,
            deferred_tree_refresh: false,
        }
    }

    pub(crate) fn install(&mut self, watcher: RecommendedWatcher) {
        self.watcher = Some(watcher);
    }

    pub(super) fn take_ready_batch(&mut self) -> Option<FileChangeBatch> {
        let mut immediate = false;
        while let Ok(event) = self.change_rx.try_recv() {
            self.pending
                .entry(event.path)
                .and_modify(|tree_changed| *tree_changed |= event.tree_changed)
                .or_insert(event.tree_changed);
            immediate |= event.immediate;
            self.ready_at = Some(Instant::now() + Self::DEBOUNCE);
        }
        if self.pending.is_empty()
            || (!immediate
                && self
                    .ready_at
                    .is_some_and(|ready_at| Instant::now() < ready_at))
        {
            return None;
        }
        self.ready_at = None;
        let tree_changed = self.pending.values().any(|changed| *changed);
        let paths = self.pending.drain().map(|(path, _)| path).collect();
        Some(FileChangeBatch {
            paths,
            tree_changed,
        })
    }

    pub(super) fn defer_tree_refresh(&mut self) {
        self.deferred_tree_refresh = true;
    }

    pub(super) fn take_deferred_tree_refresh(&mut self) -> bool {
        std::mem::take(&mut self.deferred_tree_refresh)
    }

    #[cfg(test)]
    pub(crate) fn inject_change(&self, path: PathBuf) {
        self.inject_test_change(path, true, true);
    }

    #[cfg(test)]
    pub(crate) fn inject_test_change(&self, path: PathBuf, tree_changed: bool, immediate: bool) {
        self.change_tx
            .send(FileChangeEvent {
                path,
                tree_changed,
                immediate,
            })
            .expect("file watcher receiver should remain available during a test");
    }

    pub(super) fn sender(&self) -> Sender<FileChangeEvent> {
        self.change_tx.clone()
    }
}

/// The spatially stable keyboard regions.  This is intentionally small:
/// component-specific selection state remains with the component itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FocusBlock {
    /// The explorer's search row. Nested visually inside the same bordered
    /// box as `Files` (no standalone layout region), but a real Tab stop of
    /// its own so Tab has one consistent meaning everywhere instead of
    /// toggling a sub-mode within `Files`.
    Search,
    Files,
    Workspace,
    Sidebar,
    Composer,
    /// The footer's two controls (which-LLM, effort) — a normal Tab
    /// stop, not a separate `F3` side-channel. See `focus.rs::normalize_focus`
    /// for how `composer_chip_focus` (which of the two is selected) tracks
    /// entry/exit from this block.
    Footer,
    BottomPanel,
    /// The pending human-approval card inside the transcript. Present in the
    /// focus cycle only while a HITL request is outstanding.
    Approval,
}

impl FocusBlock {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Search => "SEARCH",
            Self::Files => "FILES",
            Self::Workspace => "CHAT",
            Self::Sidebar => "SIDEBAR",
            Self::Composer => "COMPOSER",
            Self::Footer => "FOOTER",
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
    // while a decision is pending. Footer follows Composer — the natural
    // next stop after typing is the row of dials right below it.
    pub(crate) const ORDER: [Self; 8] = [
        Self::Search,
        Self::Files,
        Self::Workspace,
        Self::Sidebar,
        Self::Approval,
        Self::Composer,
        Self::Footer,
        Self::BottomPanel,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransientOwner {
    SourceSearch,
    JumpToLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FocusMode {
    Navigation,
    Transient(TransientOwner),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExplorerNameAction {
    CreateFile,
    CreateDirectory,
    Rename,
}

#[derive(Debug, Clone)]
pub(crate) enum ExplorerDialog {
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
    DirtyExit,
    DirtySwitch {
        path: PathBuf,
    },
    SaveConflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivitySummaryModel {
    pub(crate) label: String,
    pub(crate) action_label: Option<&'static str>,
    pub(crate) kind: BannerKind,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlashCommandOrigin {
    Composer,
    GlobalPalette,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SemanticCommand {
    GoHome,
    GoBack,
    PushView(WorkspaceView),
    ReplaceView(WorkspaceView),
    OpenFile(PathBuf),
    ToggleFiles,
    CloseOverlay,
    FocusComposer,
    FocusPane(FocusBlock),
    SubmitMessage,
    InsertComposerNewline,
    OpenSlashCommands,
    OpenHelp,
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
    QuickSwitchModel,
    /// Open the persistent footer control's compact picker, focused on the
    /// given column (vendor/route, model, or effort).
    OpenModelControl(ConnectModelColumn),
    OpenBottomPanel,
    RefreshFiles,
    RefreshEditor,
    SaveEditor,
    BeginCreateFile,
    BeginCreateDirectory,
    BeginRename,
    RequestDelete,
    StartSourceSearch,
    StartJumpToLine,
    OpenExternalEditor,
    ToggleCurrentFileAttachment,
    PasteClipboardImage,
    ToggleToolDetails,
    /// Step reasoning effort one level (`Alt+,` back, `Alt+.` forward)
    /// within the current model's valid options — see
    /// [`crate::effort::ReasoningEffort::step`].
    StepReasoningEffort(bool),
    MoveQueueSelection(i32),
    CancelSelectedQueueMessage,
    MoveTasksSelection(i32),
    CancelSelectedBackgroundTask,
    ApproveSelectedBackgroundTask,
    DenySelectedBackgroundTask,
    QuitOrInterrupt,
    Quit,
}

impl SemanticCommand {
    /// Operations that must not overlap a foreground turn. The event pump
    /// still accepts navigation, composer input, queue operations and
    /// cancellation; these commands would change the active runtime route or
    /// write the workspace while a detached tool still has an in-flight view.
    pub(crate) fn available_while_busy(&self) -> bool {
        !matches!(
            self,
            Self::QuickSwitchModel
                | Self::OpenModelControl(_)
                | Self::StepReasoningEffort(_)
                | Self::OpenExternalEditor
                | Self::SaveEditor
                | Self::BeginCreateFile
                | Self::BeginCreateDirectory
                | Self::BeginRename
                | Self::RequestDelete
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TabNavCommand {
    PreviousTab,
    NextTab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FocusState {
    block: FocusBlock,
    mode: FocusMode,
    previous_block: Option<FocusBlock>,
    return_block: Option<FocusBlock>,
}

impl FocusState {
    pub(crate) fn block(&self) -> FocusBlock {
        self.block
    }

    pub(crate) fn mode(&self) -> FocusMode {
        self.mode
    }

    pub(crate) fn set_navigation(&mut self, block: FocusBlock) {
        self.block = block;
        self.mode = FocusMode::Navigation;
    }

    pub(crate) fn set_transient(&mut self, owner: TransientOwner) {
        self.block = FocusBlock::Workspace;
        self.mode = FocusMode::Transient(owner);
    }

    pub(crate) fn transition_to(&mut self, block: FocusBlock) {
        if self.block != block {
            self.previous_block = Some(self.block);
        }
        self.set_navigation(block);
        self.return_block = Some(block);
    }

    pub(crate) fn restore(&mut self, block: FocusBlock) {
        self.set_navigation(block);
        self.return_block = Some(block);
    }

    pub(crate) fn previous_block(&self) -> Option<FocusBlock> {
        self.previous_block
    }

    #[cfg(test)]
    pub(crate) fn set_previous_block_for_test(&mut self, block: Option<FocusBlock>) {
        self.previous_block = block;
    }

    #[cfg(test)]
    pub(crate) fn return_block(&self) -> Option<FocusBlock> {
        self.return_block
    }

    pub(crate) fn reset_to_workspace(&mut self) {
        self.restore(FocusBlock::Workspace);
    }
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
pub(crate) struct FocusAvailability {
    pub(crate) search: bool,
    pub(crate) files: bool,
    pub(crate) sidebar: bool,
    pub(crate) bottom_panel: bool,
    pub(crate) approval: bool,
}

impl FocusAvailability {
    pub(crate) fn contains(self, block: FocusBlock) -> bool {
        match block {
            FocusBlock::Search => self.search,
            FocusBlock::Files => self.files,
            FocusBlock::Workspace => true,
            FocusBlock::Sidebar => self.sidebar,
            FocusBlock::Composer => true,
            FocusBlock::Footer => true,
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
pub(crate) struct FooterLimits {
    pub(crate) usage: String,
    pub(crate) weekly_limit: String,
    pub(crate) credits: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationRenderKey {
    pub(crate) session_id: uuid::Uuid,
    pub(crate) width: u16,
    pub(crate) messages: usize,
    pub(crate) last_message_content: usize,
    pub(crate) last_message_thinking: usize,
    pub(crate) events: usize,
    pub(crate) last_event_detail: usize,
    pub(crate) banners: usize,
    pub(crate) queue: usize,
    pub(crate) queue_selected: Option<usize>,
    pub(crate) chat_message_start: usize,
    pub(crate) chat_event_start: usize,
    /// How many lines from the tail to materialize. Follow-mode frames only
    /// need a viewport plus overscan; scrolling up raises this. History above
    /// the window is not rebuilt.
    pub(crate) keep_from_end: usize,
    pub(crate) activity_summary: Option<(String, Option<&'static str>, BannerKind)>,
    pub(crate) tool_expanded: bool,
    pub(crate) splash_dismissed: bool,
    pub(crate) slash_mode: bool,
    pub(crate) status: forge_types::TaskLifecycle,
    pub(crate) theme_id: String,
    /// Pending HITL request identity, so the inline approval item rebuilds
    /// when a new request replaces the previous one while still `Waiting`.
    pub(crate) pending_hitl: Option<String>,
    pub(crate) approval_menu_selected: usize,
    pub(crate) approval_focused: bool,
    pub(crate) pending_question: Option<String>,
    pub(crate) question_idx: usize,
    pub(crate) question_option_idx: usize,
}

pub(crate) struct ConversationRenderCache {
    pub(crate) key: ConversationRenderKey,
    /// Shared so the render path can hold the lines without copying them. A
    /// frame clones the handle, not the ~940KB of `Line`/`Span` data behind it.
    pub(crate) lines: Arc<Vec<Line<'static>>>,
}

pub(crate) struct ConversationViewState {
    pub(crate) message_start: usize,
    pub(crate) event_start: usize,
    pub(crate) scroll: u16,
    pub(crate) follow: bool,
    pub(crate) context_reset_snapshot: Option<(f64, f64)>,
    pub(crate) splash_dismissed: bool,
}

pub(crate) struct WorkspaceFilesState {
    pub(crate) visible: bool,
    pub(crate) explorer: FileExplorer,
}

/// Owns selections for durable queued messages and background tasks.
///
/// The TUI may change the list beneath these cursors at any time, so bounds
/// maintenance belongs with the cursors rather than being duplicated by each
/// command handler.
#[derive(Default)]
pub(crate) struct TaskSelectionState {
    queue: Option<usize>,
    tasks: Option<usize>,
}

impl TaskSelectionState {
    pub(crate) fn queue(&self) -> Option<usize> {
        self.queue
    }

    pub(crate) fn task(&self) -> Option<usize> {
        self.tasks
    }

    pub(crate) fn clear_queue(&mut self) {
        self.queue = None;
    }

    pub(crate) fn ensure_queue(&mut self) {
        if self.queue.is_none() {
            self.queue = Some(0);
        }
    }

    pub(crate) fn clamp_queue(&mut self, len: usize) {
        self.queue = match (len, self.queue) {
            (0, _) => None,
            (_, Some(index)) if index < len => Some(index),
            (_, Some(_)) => Some(len - 1),
            (_, None) => Some(0),
        };
    }

    pub(crate) fn move_queue(&mut self, len: usize, delta: i32) {
        if len == 0 {
            self.queue = None;
            return;
        }
        let current = self.queue.unwrap_or(0) as i32;
        self.queue = Some((current + delta).rem_euclid(len as i32) as usize);
    }

    pub(crate) fn clamp_tasks(&mut self, len: usize) {
        self.tasks = match (len, self.tasks) {
            (0, _) => None,
            (_, Some(index)) if index < len => Some(index),
            (_, Some(_)) => Some(len - 1),
            (_, None) => Some(0),
        };
    }

    pub(crate) fn move_tasks(&mut self, len: usize, delta: i32) {
        if len == 0 {
            self.tasks = None;
            return;
        }
        let current = self.tasks.unwrap_or(0) as i32;
        self.tasks = Some((current + delta).rem_euclid(len as i32) as usize);
    }
}

pub(crate) struct StartupResumeState {
    pub(crate) picker: bool,
    pub(crate) session_id: Option<uuid::Uuid>,
}

pub(crate) struct TurnTimingState {
    pub(crate) started: Option<Instant>,
    pub(crate) thinking_started: Option<Instant>,
    pub(crate) thought_secs: Option<f64>,
}

pub(crate) struct ExternalEditorState {
    pub(crate) requested: bool,
}

#[derive(Default)]
pub(crate) struct PendingTurnState {
    prompt: Option<String>,
    continue_turn: bool,
    attachments: Vec<forge_types::ImageRef>,
}

impl PendingTurnState {
    pub(crate) fn has_prompt(&self) -> bool {
        self.prompt.is_some()
    }

    #[cfg(test)]
    pub(crate) fn prompt(&self) -> Option<&str> {
        self.prompt.as_deref()
    }

    pub(crate) fn queue(&mut self, prompt: String, attachments: Vec<forge_types::ImageRef>) {
        self.prompt = Some(prompt);
        self.attachments = attachments;
    }

    pub(crate) fn request_continue(&mut self) {
        self.continue_turn = true;
    }

    pub(crate) fn continue_requested(&self) -> bool {
        self.continue_turn
    }

    pub(crate) fn take(&mut self) -> (Option<String>, bool, Vec<forge_types::ImageRef>) {
        (
            self.prompt.take(),
            std::mem::take(&mut self.continue_turn),
            std::mem::take(&mut self.attachments),
        )
    }

    pub(crate) fn clear(&mut self) {
        self.prompt = None;
        self.continue_turn = false;
        self.attachments.clear();
    }

    #[cfg(test)]
    pub(crate) fn attachment_count(&self) -> usize {
        self.attachments.len()
    }
}

/// Deferred interactions owned by the event loop.
///
/// A drain consumes its pending flag before doing async work so the same
/// interaction cannot be re-entered by another loop iteration.
#[derive(Default)]
pub(crate) struct PendingInteractionState {
    hitl_decision: Option<HitlDecision>,
    hitl_remember: bool,
    question_submit: Option<questions::QuestionSubmit>,
    context_reset: bool,
}

impl PendingInteractionState {
    pub(crate) fn has_hitl_decision(&self) -> bool {
        self.hitl_decision.is_some()
    }

    pub(crate) fn request_hitl_decision(&mut self, decision: HitlDecision, remember: bool) {
        self.hitl_decision = Some(decision);
        self.hitl_remember = remember;
    }

    pub(crate) fn take_hitl_decision(&mut self) -> Option<(HitlDecision, bool)> {
        self.hitl_decision.take().map(|decision| {
            let remember = std::mem::take(&mut self.hitl_remember);
            (decision, remember)
        })
    }

    pub(crate) fn has_question_submit(&self) -> bool {
        self.question_submit.is_some()
    }

    pub(crate) fn request_question_submit(&mut self, submit: questions::QuestionSubmit) {
        self.question_submit = Some(submit);
    }

    pub(crate) fn take_question_submit(&mut self) -> Option<questions::QuestionSubmit> {
        self.question_submit.take()
    }

    pub(crate) fn context_reset_pending(&self) -> bool {
        self.context_reset
    }

    pub(crate) fn request_context_reset(&mut self) {
        self.context_reset = true;
    }

    /// Returns whether a reset was pending and clears it atomically.
    pub(crate) fn take_context_reset(&mut self) -> bool {
        std::mem::take(&mut self.context_reset)
    }

    pub(crate) fn clear(&mut self) {
        self.hitl_decision = None;
        self.hitl_remember = false;
        self.question_submit = None;
        self.context_reset = false;
    }
}

#[derive(Default)]
pub(crate) struct AttachmentState {
    pending: Option<forge_workspace::file_context::FileAttachment>,
    pending_images: Vec<forge_types::ImageRef>,
}

impl AttachmentState {
    pub(crate) fn file(&self) -> Option<&forge_workspace::file_context::FileAttachment> {
        self.pending.as_ref()
    }

    pub(crate) fn file_mut(
        &mut self,
    ) -> Option<&mut forge_workspace::file_context::FileAttachment> {
        self.pending.as_mut()
    }

    pub(crate) fn set_file(&mut self, attachment: forge_workspace::file_context::FileAttachment) {
        self.pending = Some(attachment);
    }

    pub(crate) fn take_file(&mut self) -> Option<forge_workspace::file_context::FileAttachment> {
        self.pending.take()
    }

    pub(crate) fn clear_file(&mut self) {
        self.pending = None;
    }

    pub(crate) fn has_images(&self) -> bool {
        !self.pending_images.is_empty()
    }

    pub(crate) fn image_count(&self) -> usize {
        self.pending_images.len()
    }

    pub(crate) fn images(&self) -> &[forge_types::ImageRef] {
        &self.pending_images
    }

    pub(crate) fn push_image(&mut self, image: forge_types::ImageRef) {
        self.pending_images.push(image);
    }

    pub(crate) fn pop_image(&mut self) -> Option<forge_types::ImageRef> {
        self.pending_images.pop()
    }

    pub(crate) fn take_images(&mut self) -> Vec<forge_types::ImageRef> {
        std::mem::take(&mut self.pending_images)
    }
}

#[derive(Default)]
pub(crate) struct ExplorerDialogState {
    current: Option<ExplorerDialog>,
}

impl ExplorerDialogState {
    pub(crate) fn current(&self) -> Option<&ExplorerDialog> {
        self.current.as_ref()
    }

    pub(crate) fn current_mut(&mut self) -> Option<&mut ExplorerDialog> {
        self.current.as_mut()
    }

    pub(crate) fn is_open(&self) -> bool {
        self.current.is_some()
    }

    pub(crate) fn show(&mut self, dialog: ExplorerDialog) {
        self.current = Some(dialog);
    }

    pub(crate) fn take(&mut self) -> Option<ExplorerDialog> {
        self.current.take()
    }

    pub(crate) fn replace(&mut self, dialog: Option<ExplorerDialog>) {
        self.current = dialog;
    }

    pub(crate) fn clear(&mut self) {
        self.current = None;
    }
}

#[derive(Default)]
pub(crate) struct CancellationState {
    requested: bool,
}

impl CancellationState {
    pub(crate) fn is_requested(&self) -> bool {
        self.requested
    }

    /// Returns whether cancellation was pending and clears it atomically.
    pub(crate) fn take_requested(&mut self) -> bool {
        std::mem::take(&mut self.requested)
    }

    pub(crate) fn request(&mut self) -> bool {
        if self.requested {
            false
        } else {
            self.requested = true;
            true
        }
    }

    pub(crate) fn clear(&mut self) {
        self.requested = false;
    }
}

#[derive(Default)]
pub(crate) struct ToolDetailState {
    expanded: bool,
}

impl ToolDetailState {
    pub(crate) fn is_expanded(&self) -> bool {
        self.expanded
    }

    pub(crate) fn toggle(&mut self) {
        self.expanded = !self.expanded;
    }

    pub(crate) fn collapse(&mut self) {
        self.expanded = false;
    }
}

pub(crate) struct SearchStatusState {
    pub(crate) label: Option<String>,
}

pub(crate) struct ReasoningEffortState {
    pub(crate) value: ReasoningEffort,
}

pub(crate) struct EditorViewportState {
    pub(crate) height: u16,
}

pub(crate) struct SlashSuggestionState {
    pub(crate) selected: usize,
}

pub(crate) struct ExitState {
    requested: bool,
    code: ExitCode,
}

impl Default for ExitState {
    fn default() -> Self {
        Self {
            requested: false,
            code: ExitCode::Success,
        }
    }
}

impl ExitState {
    pub(crate) fn is_requested(&self) -> bool {
        self.requested
    }

    pub(crate) fn code(&self) -> ExitCode {
        self.code
    }

    pub(crate) fn request(&mut self) {
        self.requested = true;
    }

    pub(crate) fn set_code(&mut self, code: ExitCode) {
        self.code = code;
    }

    pub(crate) fn request_with_code(&mut self, code: ExitCode) {
        self.request();
        self.set_code(code);
    }
}

#[derive(Default)]
pub(crate) struct ToastState {
    current: Option<(Instant, String)>,
}

impl ToastState {
    pub(crate) fn show(&mut self, text: impl Into<String>) -> String {
        let text = text.into();
        self.current = Some((Instant::now(), text.clone()));
        text
    }

    pub(crate) fn clear(&mut self) {
        self.current = None;
    }

    pub(crate) fn expire(&mut self, timeout: Duration) {
        if self
            .current
            .as_ref()
            .is_some_and(|(shown_at, _)| shown_at.elapsed() > timeout)
        {
            self.current = None;
        }
    }
}

pub(crate) struct NoticeState {
    pub(crate) items: Vec<String>,
    pub(crate) until: Option<Instant>,
}

pub(crate) struct BannerState {
    pub(crate) items: Vec<ChatItem>,
}

pub(crate) struct RenderCacheState {
    pub(crate) conversation: Option<ConversationRenderCache>,
}

pub(crate) struct BusyState {
    active: bool,
    phase: BusyPhase,
}

impl Default for BusyState {
    fn default() -> Self {
        Self {
            active: false,
            phase: BusyPhase::Idle,
        }
    }
}

impl BusyState {
    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn phase(&self) -> BusyPhase {
        self.phase.clone()
    }

    pub(crate) fn start(&mut self, phase: BusyPhase) {
        self.active = true;
        self.phase = phase;
    }

    pub(crate) fn set_phase(&mut self, phase: BusyPhase) {
        self.phase = phase;
    }

    pub(crate) fn activate(&mut self) {
        self.active = true;
    }

    pub(crate) fn stop(&mut self) {
        self.active = false;
        self.phase = BusyPhase::Idle;
    }
}

pub(crate) struct StatusMessageState {
    pub(crate) message: String,
}

pub(crate) struct StreamState {
    pub(crate) preview: String,
    pub(crate) thinking: String,
    pub(crate) live_lines: Option<(u16, usize, usize, Arc<Vec<Line<'static>>>)>,
    /// When the preview last rebuilt at a *new width*. Tail-only rebuilds are
    /// cheap enough to run every frame; a width change re-renders the settled
    /// prefix too, and terminals emit one resize event per column during a
    /// drag, so those are debounced.
    pub(crate) last_preview_render: Option<Instant>,
    /// Settled prefix of the streaming answer, so a rebuild re-parses only the
    /// tail. See `StreamMarkdownCache`.
    pub(crate) markdown: crate::conversation::StreamMarkdownCache,
}
/// Composer placeholder on an empty workspace.
pub(crate) const COMPOSER_OPENER: &str = "What does this project do?";

/// Composer placeholder once a turn has run.
pub(crate) const COMPOSER_WORKING: &str = "Reply, or describe the next task…";

pub struct TuiApp {
    pub(crate) session: AgentSession,
    /// Per-frame view of `session`, refreshed at the top of `draw`. Render
    /// paths read this instead of the live session, so what they need stops
    /// depending on who owns it.
    pub(crate) session_view: SessionSnapshot,
    /// The transcript behind an `Arc`, re-copied only when it actually
    /// changes. Separate from `session_view` because it is the expensive
    /// half and not every caller of that one wants it.
    pub(crate) transcript_view: TranscriptSnapshot,
    pub(crate) input: InputModel,
    pub(crate) overlay: Option<Overlay>,
    /// Esc on connect overlays exits the process (first-install / resume-at-provider).
    pub(crate) onboarding_connect: bool,
    pub(crate) exit: ExitState,
    pub(crate) startup_resume: StartupResumeState,
    pub(crate) busy_state: BusyState,
    pub(crate) status_state: StatusMessageState,
    pub(crate) runtime: TuiRuntimeConfig,
    pub(crate) connect: connect::ConnectionModel,
    /// Phase 7 — submitted command history (Up/Down when no overlay).
    pub(crate) history: InputHistory,
    pub(crate) slash_suggestions: SlashSuggestionState,
    pub(crate) notice_state: NoticeState,
    /// Phase 10 / TUI-08 — always-visible feedback strip model.
    pub(crate) feedback: FeedbackModel,
    pub(crate) feedback_until: Option<Instant>,
    pub(crate) banner_state: BannerState,
    /// Phase 10 / TUI-10 — progressive busy phase for chrome.
    pub(crate) pending_turn: PendingTurnState,
    pub(crate) pending_interaction: PendingInteractionState,
    /// External-editor request queued for the event loop (terminal suspend/resume).
    pub(crate) external_editor: ExternalEditorState,
    /// Approved HITL tool running off the event loop so frames keep painting.
    pub(crate) pending_approved_tool: Option<IsolatedTask<forge_core::CompletedHitlExecution>>,
    /// Wake-driven terminal input for the full-screen runtime. Tests inject
    /// events through `test_events` instead of opening the process TTY.
    pub(crate) terminal_events: Option<super::shell::TerminalEventSource>,
    /// Synthetic terminal events used by responsiveness tests. Production
    /// input comes from `terminal_events`.
    #[cfg(test)]
    pub(crate) test_events: std::collections::VecDeque<Event>,
    pub(crate) attachment: AttachmentState,
    /// Selected queued row for keyboard cancellation.
    pub(crate) task_selection: TaskSelectionState,
    /// Live assistant text while tokens stream in.
    pub(crate) stream: StreamState,
    pub(crate) timing: TurnTimingState,
    pub(crate) search_status: SearchStatusState,
    /// Phase 10 / TUI-10 — activity ring buffer.
    pub(crate) activity: ActivityFeed,
    pub(crate) reasoning_effort: ReasoningEffortState,
    /// When `Some`, composer chip bar is focused at this index.
    pub(crate) composer_chip_focus: Option<usize>,
    pub(crate) tool_detail: ToolDetailState,
    /// V3.1 contextual workspace navigation.
    pub(crate) workspace_navigation: WorkspaceNavigation,
    /// Read-only source viewer state for the File workspace view.
    pub(crate) source_viewer: SourceViewer,
    /// Editing state staged for the editable workspace editor.
    #[allow(dead_code)] // Consumed when the editor rendering/input migration lands.
    pub(crate) editor_session: Option<EditorSession>,
    /// Active Vim-style command line, without the leading `:`.
    pub(crate) editor_command: Option<String>,
    /// Last Vim-style editor result, cleared by the next keypress.
    pub(crate) editor_message: Option<String>,
    pub(crate) pending_editor_path: Option<PathBuf>,
    pub(crate) pending_editor_home: bool,
    pub(crate) file_watch: FileWatchState,
    pub(crate) bottom_panel: BottomPanelState,
    pub(crate) workspace_files: WorkspaceFilesState,
    pub(crate) explorer_dialog: ExplorerDialogState,
    /// Authoritative keyboard ownership. Legacy component `focused` flags are
    /// synchronised from this state for rendering only.
    pub(crate) focus: FocusState,
    pub(crate) cancellation: CancellationState,
    /// Approval state is owned by `app::approvals`; other TUI areas interact
    /// with it through `TuiApp` methods instead of its internal collections.
    pub(crate) approval_session: approvals::ApprovalSessionState,
    pub(crate) question_session: questions::QuestionSessionState,
    pub(crate) toast: ToastState,
    pub(crate) editor_viewport: EditorViewportState,
    /// Session message/event offsets hidden by the most recent `/clear`.
    pub(crate) conversation_view: ConversationViewState,
    pub(crate) render_cache: RenderCacheState,
    /// Last known repo header. Refreshed off-thread by `poll_repo_header`; the
    /// render path only ever reads it, never derives it.
    pub(crate) repo_header_state: RepoHeaderState,
    /// Cached durable progress, refreshed by the event loop rather than draw.
    pub(crate) progress_state: ProgressState,
    pub(crate) interactive_terminal: Option<InteractiveTerminal>,
    /// Editor pane's terminal rect from the most recent draw, used for mouse
    /// hit-testing (mouse events arrive between frames).
    pub(crate) editor_area: Option<ratatui::layout::Rect>,
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
    pub(crate) terminal_area: Option<ratatui::layout::Rect>,
    pub(crate) terminal_rows: Vec<String>,
    pub(crate) catalog_fetch: CatalogFetchState,
    /// Frame width from the most recent draw. Key handling runs before the
    /// next render, so commands that depend on whether a pane can physically
    /// fit (e.g. the explorer toggle) need last frame's width to answer.
    pub(crate) last_frame_width: u16,
}

#[derive(Debug, Clone)]
pub(crate) struct RepoHeaderCache {
    pub(crate) repo_name: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) dirty: bool,
}

pub(crate) struct RepoHeaderState {
    pub(crate) cache: RepoHeaderCache,
    pub(crate) refresh_rx: Option<std::sync::mpsc::Receiver<RepoHeaderCache>>,
    pub(crate) refreshed_at: Instant,
    /// Directory the cached header describes, so a cwd change invalidates it.
    pub(crate) cwd: PathBuf,
}

#[derive(Debug, Default)]
pub(crate) struct ProgressState {
    pub(crate) path: Option<PathBuf>,
    pub(crate) modified: Option<std::time::SystemTime>,
    pub(crate) description: Option<String>,
    /// Last metadata poll. The event loop runs five times a second even while
    /// idle; durable progress only needs a low-frequency fallback because the
    /// workspace watcher refreshes all other UI state promptly.
    pub(crate) last_checked: Option<Instant>,
}

/// Off-thread model-catalog refresh, matching `RepoHeaderState`'s
/// spawn-a-thread-and-poll shape. The worker thread refreshes
/// `ModelCatalogCache`'s on-disk file as a side effect and reports back only
/// success/failure — callers re-read the (now warm) cache via the existing
/// synchronous `model_picker_items(false)` rather than threading fetched data
/// through the channel, so there is never a second in-memory catalog that
/// could drift from the disk cache `models_for_picker` already owns.
pub(crate) struct CatalogFetchState {
    pub(crate) refresh_rx: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    /// Set once the first background refresh has ever been kicked off this
    /// session, so the lazy first-render warm-up in `draw()` (see
    /// `footer_has_compact_control`) fires at most once per app lifetime —
    /// every later refresh is triggered explicitly by opening a picker.
    pub(crate) warmed: bool,
}
