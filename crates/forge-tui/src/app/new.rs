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
        // One synchronous read at startup so the first frame shows the real branch
        // instead of blanking until the first background refresh lands.
        let repo_header_cwd = runtime.cwd.clone();
        let repo_header = chrome::load_repo_header(&repo_header_cwd);
        let mut app = Self {
            // Captured before the first `draw` so the first frame reads a real
            // snapshot rather than the empty default.
            task_chrome: vec![TaskChromeItem {
                session_id: session.session_id,
                slot: Some(1),
                label: runtime
                    .cwd
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("primary")
                    .to_string(),
                branch: repo_header.branch.clone().unwrap_or_else(|| "HEAD".into()),
                lifecycle: session.active_task.lifecycle,
                selected: true,
                secondary: None,
                attention: false,
            }],
            task_strip_selection: 0,
            selected_task_id: session.session_id,
            task_view_states: std::collections::HashMap::new(),
            supervisor: None,
            session_view: SessionSnapshot::capture(&session),
            transcript_view: TranscriptSnapshot::capture(&session),
            session,
            input,
            overlay: startup_items.clone().map(Overlay::resume_picker),
            onboarding_connect: false,
            exit: ExitState::default(),
            startup_resume: StartupResumeState {
                picker: startup_items.is_some(),
                session_id: startup_resume_session_id,
            },
            busy_state: BusyState::default(),
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
            search_status: SearchStatusState { label: None },
            activity: ActivityFeed::default(),
            pending_turn: PendingTurnState::default(),
            pending_interaction: PendingInteractionState::default(),
            external_editor: ExternalEditorState { requested: false },
            pending_approved_tool: None,
            terminal_events: None,
            #[cfg(test)]
            test_events: std::collections::VecDeque::new(),
            attachment: AttachmentState::default(),
            task_selection: TaskSelectionState::default(),
            stream: StreamState {
                preview: String::new(),
                revealed: 0,
                revealed_at: None,
                thinking: String::new(),
                live_lines: None,
                last_preview_render: None,
                markdown: Default::default(),
            },
            timing: TurnTimingState {
                started: None,
                turn_started: None,
                thinking_started: None,
                thought_secs: None,
                chars: 0,
                tools: 0,
                completion_tokens_at_start: 0,
            },
            reasoning_effort: ReasoningEffortState {
                value: ReasoningEffort::Auto,
            },
            composer_chip_focus: None,
            tool_detail: ToolDetailState::default(),
            turn_expansion: TurnExpansionState::default(),
            turn_stats: std::collections::HashMap::new(),
            workspace_navigation: WorkspaceNavigation::default(),
            source_viewer: SourceViewer::new(),
            diff_view: crate::diff_view::DiffView::default(),
            diff_explorer_was_visible: None,
            editor_session: None,
            editor_command: None,
            editor_message: None,
            pending_editor_path: None,
            pending_editor_home: false,
            file_watch: FileWatchState::new(),
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
            cancellation: CancellationState::default(),
            approval_session: approvals::ApprovalSessionState::default(),
            question_session: questions::QuestionSessionState::default(),
            toast: ToastState::default(),
            conversation_view: ConversationViewState {
                message_start: 0,
                event_start: 0,
                scroll: 0,
                follow: true,
                context_reset_snapshot: None,
                splash_dismissed: false,
            },
            render_cache: RenderCacheState { conversation: None },
            repo_header_state: RepoHeaderState {
                cache: repo_header,
                refresh_rx: None,
                refreshed_at: Instant::now(),
                cwd: repo_header_cwd.clone(),
            },
            progress_state: ProgressState::default(),
            interactive_terminal: None,
            editor_viewport: EditorViewportState { height: 24 },
            editor_area: None,
            composer_area: None,
            selection: crate::selection::MouseSelection::default(),
            context_menu: None,
            conversation_area: None,
            conversation_rows: Vec::new(),
            terminal_area: None,
            terminal_rows: Vec::new(),
            catalog_fetch: CatalogFetchState {
                refresh_rx: None,
                warmed: false,
            },
            // 0 until the first draw: "unknown", which the explorer toggle
            // treats as "don't refuse" rather than guessing a width.
            last_frame_width: 0,
        };
        app.init_file_watcher();
        app.load_ui_state();
        app.restore_saved_auth().apply_connection_chrome()
    }
}
