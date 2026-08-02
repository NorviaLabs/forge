//! `TuiApp` construction.
//!
//! Split out of `app/mod.rs` per #19. Startup wiring only — field defaults and
//! the initial load of run history, UI state, and saved auth. Moved verbatim.

use super::*;

impl TuiApp {
    pub fn new(session: AgentSession, runtime: TuiRuntimeConfig) -> Self {
        Self::new_with_startup_resume_picker(session, runtime, None)
    }

    pub fn new_with_startup_resume_picker(
        session: AgentSession,
        runtime: TuiRuntimeConfig,
        startup_items: Option<Vec<ResumeSessionItem>>,
    ) -> Self {
        let startup_resume_session_id = startup_items.as_ref().map(|_| session.session_id);
        let workspace_root = session.workspace_root().to_path_buf();
        let (registry, theme_notices) =
            crate::theme_registry::ThemeRegistry::load_with_diagnostics(Some(&workspace_root));
        let theme_id = registry.resolve_startup_id(&runtime.theme_id);
        crate::theme::install(registry, theme_id);
        let mut input = InputModel::default();
        input.hint = "Describe a task…".into();
        let mut startup_notices = runtime.startup_notices.clone();
        startup_notices.extend(theme_notices);
        let file_icons = runtime.file_icons;
        let run = RunStateModel::new(workspace_root.clone(), runtime.validation_command.clone());
        let (file_change_tx, file_change_rx) = mpsc::channel();
        // One synchronous read at startup so the first frame shows the real branch
        // instead of blanking until the first background refresh lands.
        let repo_header_cwd = runtime.cwd.clone();
        let repo_header = chrome::load_repo_header(&repo_header_cwd);
        let mut app = Self {
            session,
            input,
            overlay: startup_items.clone().map(Overlay::resume_picker),
            exit: ExitState {
                requested: false,
                code: ExitCode::Success,
            },
            startup_resume: StartupResumeState {
                picker: startup_items.is_some(),
                session_id: startup_resume_session_id,
            },
            busy_state: BusyState {
                active: false,
                phase: BusyPhase::Idle,
            },
            status_state: StatusMessageState {
                message: String::new(),
            },
            runtime,
            connect: connect::ConnectionModel::new(),
            history: InputHistory::default(),
            slash_suggestions: SlashSuggestionState { selected: 0 },
            notice_state: NoticeState {
                items: startup_notices,
                until: None,
            },
            feedback: FeedbackModel::default(),
            banner_state: BannerState { items: Vec::new() },
            search_status: SearchStatusState {
                label: Some("mock".into()),
            },
            activity: ActivityFeed::default(),
            pending_turn: PendingTurnState {
                prompt: None,
                continue_turn: false,
            },
            pending_interaction: PendingInteractionState {
                hitl_decision: None,
                context_reset: false,
            },
            external_editor: ExternalEditorState { requested: false },
            attachment: AttachmentState { pending: None },
            task_selection: TaskSelectionState {
                queue: None,
                tasks: None,
            },
            stream: StreamState {
                preview: String::new(),
                thinking: String::new(),
            },
            timing: TurnTimingState {
                started: None,
                thinking_started: None,
                thought_secs: None,
            },
            reasoning_effort: ReasoningEffortState {
                value: ReasoningEffort::Auto,
            },
            tool_detail: ToolDetailState { expanded: false },
            workspace_navigation: WorkspaceNavigation::default(),
            source_viewer: SourceViewer::new(),
            file_watch: FileWatchState {
                watcher: None,
                change_rx: file_change_rx,
                change_tx: file_change_tx,
            },
            bottom_panel: BottomPanelState::default(),
            run,
            run_exec: run::RunExecution::default(),
            workspace_files: WorkspaceFilesState {
                visible: false,
                explorer: FileExplorer::new(Some(workspace_root), file_icons),
            },
            explorer_dialog: ExplorerDialogState::default(),
            focus: FocusState::default(),
            inspector: InspectorState {
                visible: false,
                view: InspectorView::default(),
            },
            diff_view: DiffViewState {
                selected: 0,
                snapshot: DiffSnapshot::default(),
            },
            cancellation: CancellationState { requested: false },
            hitl_session: HitlSessionState {
                allowed: HashSet::new(),
            },
            toast: ToastState { current: None },
            conversation_view: ConversationViewState {
                message_start: 0,
                event_start: 0,
                scroll: 0,
                follow: true,
                context_reset_snapshot: None,
                splash_dismissed: false,
            },
            render_cache: RenderCacheState {
                conversation: None,
                composer_layout: ComposerLayoutCache::default(),
            },
            model_cost_cache: None,
            footer_limits: FooterLimitsState {
                cache: None,
                refresh_rx: None,
            },
            repo_header_state: RepoHeaderState {
                cache: repo_header,
                refresh_rx: None,
                refreshed_at: Instant::now(),
                cwd: repo_header_cwd.clone(),
            },
            terminal_capture: TerminalCapture::default(),
            pointer: PointerState::default(),
            workspace_search: WorkspaceSearchState {
                index: None,
                error: None,
            },
            editor_viewport: EditorViewportState { height: 24 },
        };
        app.init_file_watcher();
        app.load_run_history();
        app.load_ui_state();
        app.normalize_restored_run();
        app.restore_saved_auth().apply_connection_chrome()
    }
}
