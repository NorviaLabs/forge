//! `TuiApp` construction.
//!
//! Split out of `app/mod.rs` per #19. Startup wiring only — field defaults and
//! the initial load of UI state and saved auth. Moved verbatim.

use super::*;

impl TuiApp {
    pub fn new(session: AgentSession, runtime: TuiRuntimeConfig) -> Self {
        Self::new_with_startup_resume_picker(session, runtime, None)
    }

    pub fn new_with_startup_resume_picker(
        mut session: AgentSession,
        runtime: TuiRuntimeConfig,
        startup_items: Option<Vec<ResumeSessionItem>>,
    ) -> Self {
        // Default Accept Edits (tight dev-loop shell free); load_ui_state may
        // restore a persisted mode afterward.
        session.apply_permission_mode(forge_governance::PermissionMode::AcceptEdits);
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
        let (file_change_tx, file_change_rx) = mpsc::channel();
        // One synchronous read at startup so the first frame shows the real branch
        // instead of blanking until the first background refresh lands.
        let repo_header_cwd = runtime.cwd.clone();
        let repo_header = chrome::load_repo_header(&repo_header_cwd);
        let mut app = Self {
            // Captured before the first `draw` so the first frame reads a real
            // snapshot rather than the empty default.
            session_view: SessionSnapshot::capture(&session),
            transcript_view: TranscriptSnapshot::capture(&session),
            session,
            input,
            overlay: startup_items.clone().map(Overlay::resume_picker),
            onboarding_connect: false,
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
                items: startup_notices.clone(),
                until: (!startup_notices.is_empty())
                    .then(|| Instant::now() + Duration::from_secs(7)),
            },
            feedback: if startup_notices.is_empty() {
                FeedbackModel::default()
            } else {
                FeedbackModel::info(startup_notices.join("\n"))
            },
            feedback_until: (!startup_notices.is_empty())
                .then(|| Instant::now() + Duration::from_secs(7)),
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
                live_lines: None,
                last_preview_render: None,
            },
            timing: TurnTimingState {
                started: None,
                thinking_started: None,
                thought_secs: None,
            },
            reasoning_effort: ReasoningEffortState {
                value: ReasoningEffort::Auto,
            },
            permission_mode: forge_governance::PermissionMode::AcceptEdits,
            composer_chip_focus: None,
            tool_detail: ToolDetailState { expanded: false },
            workspace_navigation: WorkspaceNavigation::default(),
            source_viewer: SourceViewer::new(),
            editor_session: None,
            editor_command: None,
            editor_message: None,
            pending_editor_path: None,
            pending_editor_home: false,
            pending_editor_diff: false,
            file_watch: FileWatchState {
                watcher: None,
                change_rx: file_change_rx,
                change_tx: file_change_tx,
            },
            bottom_panel: BottomPanelState::default(),
            workspace_files: WorkspaceFilesState {
                // Make Forge's editor/file-browser surface discoverable on a
                // first launch. A saved per-repository preference is applied
                // immediately below by `load_ui_state`.
                visible: true,
                explorer: FileExplorer::new(Some(workspace_root), file_icons),
            },
            explorer_dialog: ExplorerDialogState::default(),
            focus: FocusState::default(),
            diff_view: DiffViewState {
                selected: 0,
                snapshot: DiffSnapshot::default(),
            },
            cancellation: CancellationState { requested: false },
            hitl_session: HitlSessionState {
                allowed: HashSet::new(),
                pattern_allow: Vec::new(),
                menu: ApprovalMenuState::default(),
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
            render_cache: RenderCacheState { conversation: None },
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
            progress_state: std::cell::RefCell::new(ProgressState::default()),
            interactive_terminal: None,
            editor_viewport: EditorViewportState { height: 24 },
            editor_area: None,
            composer_area: None,
            selection: crate::selection::MouseSelection::default(),
            context_menu: None,
            conversation_area: None,
            conversation_rows: Vec::new(),
            diff_area: None,
            diff_rows: Vec::new(),
            terminal_area: None,
            terminal_rows: Vec::new(),
            catalog_fetch: CatalogFetchState {
                refresh_rx: None,
                warmed: false,
            },
        };
        app.init_file_watcher();
        app.load_ui_state();
        app.restore_saved_auth().apply_connection_chrome()
    }
}
