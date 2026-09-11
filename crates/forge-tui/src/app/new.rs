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
        let history_store = history_store_for_workspace(&workspace_root);
        let mut history = InputHistory::default();
        history.load_resumed(history_store.load(crate::history::MAX_INPUT_HISTORY));
        let mut startup_notices = runtime.startup_notices.clone();
        startup_notices.extend(theme_notices);
        let startup_banners = startup_notices
            .iter()
            .cloned()
            .map(|text| ChatItem::Banner {
                text,
                kind: BannerKind::Info,
            })
            .collect();
        let file_icons = runtime.file_icons;
        // One synchronous read at startup so the first frame shows the real branch
        // instead of blanking until the first background refresh lands.
        let repo_header_cwd = runtime.cwd.clone();
        let repo_header = chrome::load_repo_header(&repo_header_cwd);
        let mut app = Self {
            // Captured before the first `draw` so the first frame reads a real
            // snapshot rather than the empty default.
            session_chrome: vec![SessionChromeItem {
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
                updated_at: chrono::Utc::now(),
            }],
            task_strip_selection: 0,
            navigator_tab: crate::widgets::NavigatorTab::Files,
            navigator_tab_explicit: false,
            navigator_peek: None,
            navigator_reply: String::new(),
            navigator_done_pending: None,
            selected_session_id: session.session_id,
            session_view_states: std::collections::HashMap::new(),
            retiring_session_view_states: std::collections::HashMap::new(),
            supervisor: None,
            session_view: SessionSnapshot::capture(&session),
            transcript_view: TranscriptSnapshot::capture(&session),
            session_runtime: DirectSessionSlot::some(session),
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
            history,
            history_store,
            slash_suggestions: SlashSuggestionState { selected: 0 },
            feedback: FeedbackModel::default(),
            feedback_until: None,
            banner_state: BannerState {
                items: startup_banners,
            },
            turn_summaries: Vec::new(),
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
            thinking_enabled: true,
            composer_chip_focus: None,
            tool_detail: ToolDetailState::default(),
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

    pub fn new_supervised(
        initial: SessionRuntimeSnapshot,
        runtime: TuiRuntimeConfig,
        handle: SupervisorHandle,
    ) -> Self {
        let session_id = initial.task.session_id;
        let has_interrupted_prompts = !initial.interrupted_prompts.is_empty();
        let workspace_root = initial.session.workspace_root.clone();
        let (registry, theme_notices) =
            crate::theme_registry::ThemeRegistry::load_with_diagnostics(Some(&workspace_root));
        let theme_id = registry.resolve_startup_id(&runtime.theme_id);
        crate::theme::install(registry, theme_id);

        let mut input = InputModel::default();
        input.hint = "Describe a task…".into();
        let history_store = history_store_for_workspace(&workspace_root);
        let mut history = InputHistory::default();
        history.load_resumed(history_store.load(crate::history::MAX_INPUT_HISTORY));
        let mut startup_notices = runtime.startup_notices.clone();
        startup_notices.extend(theme_notices);
        let startup_banners = startup_notices
            .iter()
            .cloned()
            .map(|text| ChatItem::Banner {
                text,
                kind: BannerKind::Info,
            })
            .collect();
        let file_icons = runtime.file_icons;
        let repo_header_cwd = runtime.cwd.clone();
        let repo_header = chrome::load_repo_header(&repo_header_cwd);

        let session_view = initial.session.clone();
        let transcript_view = initial.transcript.clone();
        let session_chrome = vec![SessionChromeItem {
            session_id,
            slot: initial.task.slot,
            label: initial.task.label.clone(),
            branch: initial.task.branch.clone(),
            lifecycle: initial.session.lifecycle,
            selected: true,
            secondary: Some(initial.task.turn_state.label().into()),
            attention: has_interrupted_prompts,
            updated_at: initial.task.updated_at,
        }];
        let mut snapshots = std::collections::HashMap::new();
        snapshots.insert(session_id, initial);

        let mut app = Self {
            session_chrome,
            task_strip_selection: 0,
            navigator_tab: crate::widgets::NavigatorTab::Files,
            navigator_tab_explicit: false,
            navigator_peek: None,
            navigator_reply: String::new(),
            navigator_done_pending: None,
            selected_session_id: session_id,
            session_view_states: std::collections::HashMap::new(),
            retiring_session_view_states: std::collections::HashMap::new(),
            supervisor: Some(SupervisorUiState {
                current_session_id: session_id,
                events: handle.subscribe(),
                handle,
                snapshots,
            }),
            session_view,
            transcript_view,
            session_runtime: DirectSessionSlot::none(),
            input,
            overlay: None,
            onboarding_connect: false,
            exit: ExitState::default(),
            startup_resume: StartupResumeState {
                picker: false,
                session_id: None,
            },
            busy_state: BusyState::default(),
            status_state: StatusMessageState {
                message: String::new(),
            },
            runtime,
            connect: connect::ConnectionModel::new(),
            history,
            history_store,
            slash_suggestions: SlashSuggestionState { selected: 0 },
            feedback: FeedbackModel::default(),
            feedback_until: None,
            banner_state: BannerState {
                items: startup_banners,
            },
            turn_summaries: Vec::new(),
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
            thinking_enabled: true,
            composer_chip_focus: None,
            tool_detail: ToolDetailState::default(),
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
                cwd: repo_header_cwd,
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
            last_frame_width: 0,
        };
        app.init_file_watcher();
        app.load_ui_state();
        if has_interrupted_prompts {
            app.status_state.message = "interrupted prompt needs review".into();
            app.set_feedback(
                FeedbackSeverity::Warn,
                "A prompt was interrupted; review it before retrying",
            );
        }
        app.restore_saved_auth().apply_connection_chrome()
    }
}

fn history_store_for_workspace(workspace: &std::path::Path) -> HistoryStore {
    #[cfg(test)]
    {
        HistoryStore::new(workspace.join(".forge-test-input-history.json"), workspace)
    }
    #[cfg(not(test))]
    {
        HistoryStore::user_default(workspace)
    }
}
