//! `TuiApp` construction.
//!
//! Split out of `app/mod.rs` per #19. Startup wiring only — field defaults and
//! the initial load of run history, UI state, and saved auth. Moved verbatim.

use super::*;

impl TuiApp {
    pub fn new(session: AgentSession, runtime: TuiRuntimeConfig) -> Self {
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
            overlay: None,
            should_quit: false,
            busy: false,
            status_message: String::new(),
            runtime,
            last_exit: ExitCode::Success,
            connect: connect::ConnectionModel::new(),
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
            pending_hitl_decision: None,
            pending_context_reset: false,
            pending_external_editor: false,
            pending_attachment: None,
            queue_selected: None,
            tasks_selected: None,
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
            run_exec: run::RunExecution::default(),
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
            workspace_index: None,
            workspace_index_error: None,
            last_editor_height: 24,
        };
        app.init_file_watcher();
        app.load_run_history();
        app.load_ui_state();
        app.normalize_restored_run();
        app.restore_saved_auth().apply_connection_chrome()
    }
}
