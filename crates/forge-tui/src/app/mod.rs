//! Full-screen TUI application (TUI-01 shell + TUI-02/03/04 wired).

use std::collections::HashSet;
use std::fs;
use std::io::{self, stdout};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::conversation::ConversationRender;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    self, EnableBracketedPaste, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
use forge_connect::{
    builtin_registry, handle_connect_action, models_for_picker, needs_tui_api_key_prompt,
    needs_tui_oauth, runnable_models_for_picker, ConnectAction, ConnectError, ConnectRegistry,
    ConnectService, CredentialStore, ModelCatalogCache, ModelSelection, OauthPending,
    PreferenceStore, OPENAI_CODEX_PROFILE_ID,
};
use forge_core::{
    merge_streamed_response, observe_stream_event, AgentSession, ApplyOutcome, LoopError,
    ModelResponseApplication, ModelStepAccumulator, PendingToolApplication,
};
use forge_session::{SessionSnapshot, TranscriptSnapshot};
use forge_types::{
    HitlDecision, HitlPayload, Message, MessageRole, ModelStreamEvent, ProgressDocument,
};
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::backend::CrosstermBackend;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use ratatui::Terminal;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::json;
use thiserror::Error;

use crate::activity::{ActivityFeed, ActivityKind};
use crate::commands::{parse_slash, SlashCommand};
use crate::conversation::{
    BannerKind, ChatItem, ConversationModel, ConversationViewOpts, StreamWaitPhase,
};
use crate::editor::EditorError;
use crate::editor_session::EditorSession;
use crate::effort::ReasoningEffort;
use crate::file_explorer::{FileExplorer, FileExplorerWidget};
use crate::history::InputHistory;
use crate::interactive_terminal::InteractiveTerminal;
use crate::layout::is_too_small;
use crate::layout::{split_areas_with_chrome, split_areas_with_expanded_conversation};
use crate::overlays::{
    build_provider_rows, centered_rect, filter_palette, handle_overlay_key, models_from_catalog,
    ApprovalExecutionMode, ApprovalOverlayState, ConnectModelColumn, FileExplorerItem, Key,
    Key as OverlayKey, Overlay, OverlayAction, OverlayWidget, PaletteItem, ResumeSessionItem,
};
use crate::source_viewer::{SourceViewer, SourceViewerWidget};
use crate::terminal::TerminalGuard;
use crate::theme;
use crate::widgets::{
    classify_operator_error, composer_cursor_position, composer_text_area_width,
    footer_short_model_id, BottomPanel, BottomPanelModel, BottomPanelState, BusyPhase, FeedbackBar,
    FeedbackModel, FeedbackSeverity, FooterBar, FooterFocus, FooterModel, InputBar, InputModel,
    StatusBar, StatusModel,
};
use forge_config::FileIconMode;
use forge_workspace::file_ops::{
    DeleteMode, EntryKind, FileOperationError, FileOperationKind, WorkspaceFileOps,
};
use forge_workspace::git_status::GitStatusKind;

use crate::ExitCode;

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
mod input_route;
mod mouse;
mod new;
mod overlays;
mod persist;
/// `TuiApp::draw` lives in `app/render.rs`. Rust allows inherent `impl` blocks
/// for a type across several modules of the same crate, so this is a file split
/// only — `TuiApp`'s fields and every signature are unchanged.
mod render;
mod shell;
mod turn;
mod util;
mod watch;
mod workspace;

include!("types.inc.rs");

pub(crate) use chrome::format_exit_token_usage;
pub(crate) use chrome::recent_resume_sessions;
pub use chrome::resume_session_items;
/// Only the in-crate tests reach these directly; the runtime paths call them
/// from inside `chrome.rs`.
#[cfg(test)]
use chrome::{footer_limits_from_report, footer_usage_summary_with_cost};
pub use shell::{run_tui, run_tui_with_launch, run_tui_with_resume_picker, TuiLaunch};

#[cfg(test)]
mod tests;
