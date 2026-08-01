//! Overlays: HITL, slash palette, model picker (TUI-04).

use crate::{effort::ReasoningEffort, theme};
use forge_config::Theme;
use forge_types::HitlPayload;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Widget};
use std::path::Path;

#[derive(Debug, Clone)]
pub enum Overlay {
    Help,
    Effort {
        model: String,
        selected: usize,
        current: ReasoningEffort,
        default: ReasoningEffort,
        items: Vec<ReasoningEffort>,
    },
    StatusReport {
        title: String,
        lines: Vec<String>,
    },
    Hitl {
        payload: HitlPayload,
        approval: ApprovalOverlayState,
        /// Whether to show the expanded policy-details section.
        expanded: bool,
    },
    TurnLimit {
        turns: u32,
    },
    Model {
        provider_selected: usize,
        model_selected: usize,
        model_input: String,
        current_model: String,
        /// Provider ids (connect profile ids) derived from `groups`.
        providers: Vec<String>,
        groups: Vec<ModelGroup>,
        /// Set while disambiguating a multi-route model (Enter on a >1-route
        /// row). A sub-mode of this same overlay rather than a separate one,
        /// so Esc can return to the model list without losing search state.
        route_picker: Option<RoutePickerState>,
        /// Session's current reasoning effort, shown in the footer so effort
        /// stays visible without leaving the picker. A snapshot taken when
        /// the picker opens (mirrors `current_model`), not live-synced.
        current_effort: ReasoningEffort,
    },
    /// Phase 6.1 — OpenCode Go (and other ApiKey tui_always_prompt profiles)
    ConnectApiKey {
        profile_id: String,
        title: String,
        auth_url: Option<String>,
        /// Masked key buffer (stored plain in memory only for submit).
        key_input: String,
        /// Optional hint when env key exists
        env_hint: Option<String>,
        /// Validation/authentication error shown inside the modal.
        error: Option<String>,
    },
    /// Phase 6.1 — xAI Grok OAuth progress
    ConnectOauth {
        profile_id: String,
        title: String,
        instructions: String,
    },
    /// Phase 6/8 — pick a connect profile after `/connect`
    ConnectPicker {
        selected: usize,
        items: Vec<ConnectProfileItem>,
    },
    ResumePicker {
        selected: usize,
        items: Vec<ResumeSessionItem>,
    },
    Theme {
        selected: usize,
        current: Theme,
        items: Vec<Theme>,
    },
    FileExplorer {
        cwd: String,
        selected: usize,
        items: Vec<FileExplorerItem>,
        error: Option<String>,
    },
    FileViewer {
        path: String,
        lines: Vec<String>,
        scroll: usize,
    },
    QuickOpen {
        query: String,
        selected: usize,
        hits: Vec<QuickOpenItem>,
        error: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct FileExplorerItem {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone)]
pub struct ConnectProfileItem {
    pub id: String,
    pub title: String,
    pub auth_mode: String,
    pub auth_url: Option<String>,
    pub connected: bool,
}

#[derive(Debug, Clone)]
pub struct ResumeSessionItem {
    pub id: String,
    pub modified: String,
}

#[derive(Debug, Clone)]
pub struct QuickOpenItem {
    pub path: String,
    pub score: i32,
    pub match_ranges: Vec<(u32, u32)>,
}

#[derive(Debug, Clone)]
pub struct PaletteItem {
    pub cmd: String,
    pub desc: String,
}

#[derive(Debug, Clone)]
pub struct ModelItem {
    pub provider: String,
    pub model: String,
    /// Optional connect profile that sourced this row (catalog).
    pub profile_id: Option<String>,
    /// Whether this is account-verified or public registry metadata.
    pub source: forge_connect::CatalogSource,
}

/// One user-facing model in the picker, grouped from every [`ModelItem`] route
/// that offers it (by bare model name) so a model with several provider routes
/// renders as a single row instead of one row per route.
#[derive(Debug, Clone)]
pub struct ModelGroup {
    /// Bare model name shared by every route, e.g. `gpt-5.6` (no provider prefix).
    pub model_id: String,
    pub routes: Vec<ModelItem>,
    /// Tab-reveal state: whether routes are shown inline under this row.
    pub expanded: bool,
}

/// Active state while disambiguating a multi-route model in the picker.
#[derive(Debug, Clone)]
pub struct RoutePickerState {
    pub model_id: String,
    pub selected: usize,
    pub routes: Vec<ModelItem>,
}

/// Group picker rows by their bare model name, preserving every route.
fn group_model_items(items: Vec<ModelItem>) -> Vec<ModelGroup> {
    let mut out: Vec<ModelGroup> = Vec::new();
    let mut index: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for item in items {
        let model_id = forge_connect::route_model_id(&item.model).to_string();
        match index.get(&model_id) {
            Some(&i) => out[i].routes.push(item),
            None => {
                index.insert(model_id.clone(), out.len());
                out.push(ModelGroup {
                    model_id,
                    routes: vec![item],
                    expanded: false,
                });
            }
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalExecutionMode {
    Direct,
    Shell,
}

impl ApprovalExecutionMode {
    fn label(self) -> &'static str {
        match self {
            Self::Direct => "Direct",
            Self::Shell => "Shell",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalFocusedAction {
    AllowOnce,
    RememberDirect,
    Deny,
}

#[derive(Debug, Clone)]
pub struct ApprovalOverlayState {
    pub mode: ApprovalExecutionMode,
    pub executable_or_shell: String,
    pub arguments: Vec<String>,
    pub shell_command: Option<String>,
    pub working_directory: String,
    pub environment_delta: String,
    pub source: String,
    pub remember_eligible: bool,
    pub focused_action: ApprovalFocusedAction,
}

impl ApprovalOverlayState {
    pub fn for_payload(payload: &HitlPayload, working_directory: impl Into<String>) -> Self {
        let fallback_working_directory = working_directory.into();
        let mode = approval_mode_for_tool(&payload.tool);
        let executable_or_shell = approval_executable_for_tool(&payload.tool);
        let arguments = approval_argument_vector(&payload.tool, &payload.args_redacted);
        let shell_command = (mode == ApprovalExecutionMode::Shell)
            .then(|| {
                payload
                    .args_redacted
                    .get("command")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_owned()
            })
            .filter(|value| !value.is_empty());
        let environment_delta = approval_environment_delta(&payload.args_redacted);
        let remember_eligible = mode == ApprovalExecutionMode::Direct
            && !contains_redacted_value(&payload.args_redacted)
            && environment_delta != "[REDACTED]";

        Self {
            mode,
            executable_or_shell,
            arguments,
            shell_command,
            working_directory: approval_working_directory(
                &payload.args_redacted,
                fallback_working_directory,
            ),
            environment_delta,
            source: "Agent suggestion".into(),
            remember_eligible,
            focused_action: ApprovalFocusedAction::AllowOnce,
        }
    }

    fn focus_next(&mut self, delta: i32) {
        let actions = self.actions();
        if actions.is_empty() {
            self.focused_action = ApprovalFocusedAction::AllowOnce;
            return;
        }
        let current = actions
            .iter()
            .position(|action| *action == self.focused_action)
            .unwrap_or(0);
        let next = (current as i32 + delta).rem_euclid(actions.len() as i32) as usize;
        self.focused_action = actions[next];
    }

    fn actions(&self) -> Vec<ApprovalFocusedAction> {
        let mut actions = vec![
            ApprovalFocusedAction::AllowOnce,
            ApprovalFocusedAction::Deny,
        ];
        if self.remember_eligible {
            actions.insert(1, ApprovalFocusedAction::RememberDirect);
        }
        actions
    }
}

pub fn default_palette_items() -> Vec<PaletteItem> {
    // Keep in sync with `commands::parse_slash`.
    vec![
        PaletteItem {
            cmd: "/connect".into(),
            desc: "Connect provider (xAI, OpenCode Go/Zen, OpenAI, Anthropic, Ollama)".into(),
        },
        PaletteItem {
            cmd: "/model".into(),
            desc: "Switch model for future turns".into(),
        },
        PaletteItem {
            cmd: "/theme".into(),
            desc: "Switch presentation theme".into(),
        },
        PaletteItem {
            cmd: "/compact".into(),
            desc: "Continue in a fresh context".into(),
        },
        PaletteItem {
            cmd: "/resume".into(),
            desc: "Restore a previous session".into(),
        },
        PaletteItem {
            cmd: "/clear".into(),
            desc: "Clear the TUI screen".into(),
        },
        PaletteItem {
            cmd: "/disconnect".into(),
            desc: "Log out and clear credentials".into(),
        },
        PaletteItem {
            cmd: "/quit".into(),
            desc: "Exit TUI".into(),
        },
    ]
}

/// Model list from cached provider catalogs (no network).
pub fn default_models() -> Vec<ModelItem> {
    let mut items = Vec::new();
    for p in forge_connect::builtin_registry().profiles() {
        // Dedup within this profile's own default list only — two profiles
        // offering the same model id are distinct, independently reachable
        // routes, not duplicates (see forge_connect::group_routes).
        let mut seen = std::collections::BTreeSet::new();
        for m in &p.default_models {
            if seen.insert(m.clone()) {
                items.push(ModelItem {
                    provider: "native".into(),
                    model: m.clone(),
                    profile_id: Some(p.id.clone()),
                    source: forge_connect::CatalogSource::Default,
                });
            }
        }
    }
    items
}

/// Build picker rows from live or cached catalog entries.
pub fn models_from_catalog(entries: &[forge_connect::CatalogEntry]) -> Vec<ModelItem> {
    entries
        .iter()
        .map(|e| ModelItem {
            provider: "native".into(),
            model: e.id.clone(),
            profile_id: Some(e.profile_id.clone()),
            source: e.source,
        })
        .collect()
}

fn effort_options(model: &str) -> Vec<ReasoningEffort> {
    ReasoningEffort::options_for_model(model)
}

impl Overlay {
    pub fn welcome() -> Self {
        Self::Help
    }

    pub fn model_open() -> Self {
        Self::model_open_with(default_models())
    }

    pub fn model_open_with(items: Vec<ModelItem>) -> Self {
        let mut providers: Vec<String> = items
            .iter()
            .filter_map(|m| {
                if let Some(ref pid) = m.profile_id {
                    Some(pid.clone())
                } else {
                    let pfx = m.model.split('/').next().unwrap_or("").trim();
                    if pfx.is_empty() {
                        None
                    } else {
                        Some(pfx.to_string())
                    }
                }
            })
            .collect();
        providers.sort();
        providers.dedup();
        if providers.is_empty() {
            providers.push("all".into());
        }
        Self::Model {
            provider_selected: 0,
            model_selected: 0,
            model_input: String::new(),
            current_model: String::new(),
            providers,
            groups: group_model_items(items),
            route_picker: None,
            current_effort: ReasoningEffort::default(),
        }
    }

    /// Snapshot the session's current effort into an open model picker, so
    /// its footer reflects the real value instead of the `Auto` default.
    pub fn set_current_effort(&mut self, effort: ReasoningEffort) {
        if let Self::Model { current_effort, .. } = self {
            *current_effort = effort;
        }
    }

    pub fn effort_open(model: impl Into<String>, current: ReasoningEffort) -> Self {
        let model = model.into();
        let items = effort_options(&model);
        let default = ReasoningEffort::default_for_model(&model);
        let selected = items
            .iter()
            .position(|item| *item == current)
            .or_else(|| items.iter().position(|item| *item == default))
            .unwrap_or(0);
        Self::Effort {
            model,
            selected,
            current,
            default,
            items,
        }
    }

    /// Focus the model picker on the given model id (best-effort).
    pub fn focus_model(&mut self, model_id: &str) {
        let needle = model_id.trim();
        if needle.is_empty() {
            return;
        }
        let Self::Model {
            provider_selected,
            model_selected,
            current_model,
            providers,
            groups,
            route_picker,
            ..
        } = self
        else {
            return;
        };
        *current_model = needle.to_string();
        *route_picker = None;

        // Find exact model first.
        if let Some(found) = groups
            .iter()
            .find(|g| g.routes.iter().any(|m| m.model == needle))
        {
            let pid = found
                .routes
                .iter()
                .find(|m| m.model == needle)
                .and_then(|m| m.profile_id.clone())
                .unwrap_or_else(|| needle.split('/').next().unwrap_or("").to_string());
            if let Some(pi) = providers.iter().position(|p| p == &pid) {
                *provider_selected = pi;
            }
            let active_pid = providers
                .get(*provider_selected)
                .map(|s| s.as_str())
                .unwrap_or("all");
            let found_model_id = found.model_id.clone();
            if let Some(gi) = groups
                .iter()
                .filter(|g| group_matches_provider(active_pid, g))
                .position(|g| g.model_id == found_model_id)
            {
                *model_selected = gi;
            }
            return;
        }

        // Fallback: focus provider by prefix.
        let prefix = needle.split('/').next().unwrap_or("").trim();
        if prefix.is_empty() {
            return;
        }
        if let Some(pi) = providers.iter().position(|p| p == prefix) {
            *provider_selected = pi;
            *model_selected = 0;
        }
    }

    pub fn hitl(payload: HitlPayload) -> Self {
        Self::hitl_with_working_directory(payload, "workspace")
    }

    pub fn hitl_with_working_directory(
        payload: HitlPayload,
        working_directory: impl Into<String>,
    ) -> Self {
        let approval = ApprovalOverlayState::for_payload(&payload, working_directory);
        Self::Hitl {
            payload,
            approval,
            expanded: false,
        }
    }

    pub fn turn_limit(turns: u32) -> Self {
        Self::TurnLimit { turns }
    }

    pub fn connect_api_key(
        profile_id: impl Into<String>,
        title: impl Into<String>,
        auth_url: Option<String>,
        env_hint: Option<String>,
    ) -> Self {
        Self::ConnectApiKey {
            profile_id: profile_id.into(),
            title: title.into(),
            auth_url,
            key_input: String::new(),
            env_hint,
            error: None,
        }
    }

    pub fn connect_oauth(
        profile_id: impl Into<String>,
        title: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Self {
        Self::ConnectOauth {
            profile_id: profile_id.into(),
            title: title.into(),
            instructions: instructions.into(),
        }
    }

    pub fn connect_picker(items: Vec<ConnectProfileItem>) -> Self {
        let mut items = items;
        items.sort_by(|a, b| {
            a.title
                .to_ascii_lowercase()
                .cmp(&b.title.to_ascii_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        Self::ConnectPicker { selected: 0, items }
    }

    pub fn resume_picker(items: Vec<ResumeSessionItem>) -> Self {
        Self::ResumePicker { selected: 0, items }
    }

    pub fn theme_open(current: Theme) -> Self {
        let items = Theme::ALL.to_vec();
        let selected = items
            .iter()
            .position(|theme| *theme == current)
            .unwrap_or(0);
        Self::Theme {
            selected,
            current,
            items,
        }
    }

    pub fn file_explorer(
        cwd: impl Into<String>,
        items: Vec<FileExplorerItem>,
        error: Option<String>,
    ) -> Self {
        Self::FileExplorer {
            cwd: cwd.into(),
            selected: 0,
            items,
            error,
        }
    }

    pub fn file_viewer(path: impl Into<String>, contents: impl AsRef<str>) -> Self {
        Self::FileViewer {
            path: path.into(),
            lines: contents.as_ref().lines().map(str::to_string).collect(),
            scroll: 0,
        }
    }

    pub fn quick_open() -> Self {
        Self::QuickOpen {
            query: String::new(),
            selected: 0,
            hits: Vec::new(),
            error: None,
        }
    }

    pub fn move_sel(&mut self, delta: i32) {
        match self {
            Self::Effort {
                selected, items, ..
            } => {
                if !items.is_empty() {
                    let n = items.len() as i32;
                    *selected = ((*selected as i32 + delta).rem_euclid(n)) as usize;
                }
            }
            Self::Model {
                provider_selected,
                model_selected,
                model_input,
                providers,
                groups,
                route_picker,
                ..
            } => {
                if let Some(rp) = route_picker {
                    if rp.routes.is_empty() {
                        return;
                    }
                    let n = rp.routes.len() as i32;
                    rp.selected = ((rp.selected as i32 + delta).rem_euclid(n)) as usize;
                    return;
                }
                if groups.is_empty() {
                    return;
                }
                let pid = providers
                    .get(*provider_selected)
                    .map(|s| s.as_str())
                    .unwrap_or("all");
                let n = filtered_groups_len(pid, model_input, groups).max(1) as i32;
                *model_selected = ((*model_selected as i32 + delta).rem_euclid(n)) as usize;
            }
            Self::ConnectPicker {
                selected, items, ..
            } => {
                if items.is_empty() {
                    return;
                }
                let n = items.len() as i32;
                *selected = ((*selected as i32 + delta).rem_euclid(n)) as usize;
            }
            Self::ResumePicker {
                selected, items, ..
            } => {
                if items.is_empty() {
                    return;
                }
                let n = items.len() as i32;
                *selected = ((*selected as i32 + delta).rem_euclid(n)) as usize;
            }
            Self::Theme {
                selected, items, ..
            } => {
                if items.is_empty() {
                    return;
                }
                let n = items.len() as i32;
                *selected = ((*selected as i32 + delta).rem_euclid(n)) as usize;
            }
            Self::FileExplorer {
                selected, items, ..
            } => {
                if items.is_empty() {
                    return;
                }
                let n = items.len() as i32;
                *selected = ((*selected as i32 + delta).rem_euclid(n)) as usize;
            }
            Self::QuickOpen { selected, hits, .. } => {
                if hits.is_empty() {
                    return;
                }
                let n = hits.len() as i32;
                *selected = ((*selected as i32 + delta).rem_euclid(n)) as usize;
            }
            Self::FileViewer { scroll, lines, .. } => {
                if delta < 0 {
                    *scroll = scroll.saturating_sub(delta.unsigned_abs() as usize);
                } else {
                    *scroll = (*scroll + delta as usize).min(lines.len().saturating_sub(1));
                }
            }
            _ => {}
        }
    }
}

fn model_matches_provider(provider_id: &str, m: &ModelItem) -> bool {
    if provider_id == "all" {
        return true;
    }
    if let Some(ref pid) = m.profile_id {
        if pid == provider_id {
            return true;
        }
    }
    let pfx = m.model.split('/').next().unwrap_or("").trim();
    pfx == provider_id
}

fn model_matches_input(model_input: &str, item: &ModelItem) -> bool {
    let needle = model_input.trim().to_ascii_lowercase();
    needle.is_empty() || item.model.to_ascii_lowercase().contains(&needle)
}

fn group_matches_provider(provider_id: &str, group: &ModelGroup) -> bool {
    provider_id == "all"
        || group
            .routes
            .iter()
            .any(|m| model_matches_provider(provider_id, m))
}

fn group_matches_input(model_input: &str, group: &ModelGroup) -> bool {
    let needle = model_input.trim().to_ascii_lowercase();
    needle.is_empty()
        || group.model_id.to_ascii_lowercase().contains(&needle)
        || group
            .routes
            .iter()
            .any(|m| model_matches_input(model_input, m))
}

fn filtered_groups_len(provider_id: &str, model_input: &str, groups: &[ModelGroup]) -> usize {
    groups
        .iter()
        .filter(|g| group_matches_provider(provider_id, g) && group_matches_input(model_input, g))
        .count()
}

pub fn filter_palette(filter: &str) -> Vec<PaletteItem> {
    let f = filter.trim().trim_start_matches('/').to_ascii_lowercase();
    let mut items = default_palette_items()
        .into_iter()
        .filter(|i| {
            if f.is_empty() {
                return true;
            }
            i.cmd.to_ascii_lowercase().contains(&f) || i.desc.to_ascii_lowercase().contains(&f)
        })
        .collect::<Vec<_>>();
    items.sort_by_key(|item| {
        let command = item.cmd.trim_start_matches('/').to_ascii_lowercase();
        if command.starts_with(&f) {
            0
        } else if command.contains(&f) {
            1
        } else {
            2
        }
    });
    items
}

/// Result of handling a key inside an overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayAction {
    None,
    Close,
    BeginOnboarding,
    /// Close HITL and approve/deny
    HitlApprove,
    /// Approve and remember this exact Direct invocation for this session.
    HitlApproveSession,
    HitlDeny,
    ContinueTurns,
    StopTurns,
    /// Execute slash command string e.g. "/status"
    RunCommand(String),
    /// Model selection
    SelectModel {
        provider: String,
        model: String,
        /// The connect profile that owns the chosen route, when known — lets
        /// the app apply the exact route instead of re-guessing a profile
        /// from the model string's prefix (ambiguous once routes share ids).
        profile_id: Option<String>,
    },
    SelectEffort(ReasoningEffort),
    SelectTheme(Theme),
    /// Submit API key from ConnectApiKey overlay
    ConnectSubmitKey {
        profile_id: String,
        api_key: String,
    },
    /// Poll / continue OAuth from ConnectOauth overlay (Enter)
    ConnectCompleteOauth {
        profile_id: String,
    },
    /// Use env key without typing (secondary action on API key modal)
    ConnectUseEnv {
        profile_id: String,
    },
    /// User picked a connect profile from the picker
    ConnectPickProfile {
        profile_id: String,
    },
    FilePick {
        path: String,
        is_dir: bool,
    },
    QuickOpenFile {
        path: String,
    },
}

pub fn handle_overlay_key(overlay: &mut Overlay, key: Key) -> OverlayAction {
    match key {
        Key::Esc if matches!(overlay, Overlay::TurnLimit { .. }) => OverlayAction::StopTurns,
        Key::Esc if matches!(overlay, Overlay::Hitl { .. }) => OverlayAction::HitlDeny,
        Key::Esc
            if matches!(
                overlay,
                Overlay::Model {
                    route_picker: Some(_),
                    ..
                }
            ) =>
        {
            if let Overlay::Model { route_picker, .. } = overlay {
                *route_picker = None;
            }
            OverlayAction::None
        }
        Key::Esc => OverlayAction::Close,
        Key::Up => {
            overlay.move_sel(-1);
            OverlayAction::None
        }
        Key::Down => {
            overlay.move_sel(1);
            OverlayAction::None
        }
        Key::Left => {
            if let Overlay::Hitl { approval, .. } = overlay {
                approval.focus_next(-1);
                return OverlayAction::None;
            }
            if let Some(path) = match overlay {
                Overlay::FileExplorer { cwd, .. } => parent_dir(cwd),
                Overlay::FileViewer { path, .. } => parent_dir(path),
                _ => None,
            } {
                return OverlayAction::FilePick { path, is_dir: true };
            }
            if let Overlay::Model {
                provider_selected,
                model_selected,
                providers,
                route_picker: None,
                ..
            } = overlay
            {
                if !providers.is_empty() {
                    let n = providers.len() as i32;
                    *provider_selected = ((*provider_selected as i32 - 1).rem_euclid(n)) as usize;
                    *model_selected = 0;
                }
            }
            OverlayAction::None
        }
        Key::Right => {
            if let Overlay::Hitl { approval, .. } = overlay {
                approval.focus_next(1);
                return OverlayAction::None;
            }
            if let Overlay::Model {
                provider_selected,
                model_selected,
                providers,
                route_picker: None,
                ..
            } = overlay
            {
                if !providers.is_empty() {
                    let n = providers.len() as i32;
                    *provider_selected = ((*provider_selected as i32 + 1).rem_euclid(n)) as usize;
                    *model_selected = 0;
                }
            }
            OverlayAction::None
        }
        Key::Enter => match overlay {
            Overlay::Help => OverlayAction::BeginOnboarding,
            Overlay::Effort {
                selected, items, ..
            } => items
                .get(*selected)
                .copied()
                .map(OverlayAction::SelectEffort)
                .unwrap_or(OverlayAction::None),
            Overlay::StatusReport { .. } => OverlayAction::Close,
            Overlay::TurnLimit { .. } => OverlayAction::ContinueTurns,
            Overlay::Model {
                route_picker: Some(rp),
                ..
            } => rp
                .routes
                .get(rp.selected)
                .map(|m| OverlayAction::SelectModel {
                    provider: m.provider.clone(),
                    model: m.model.clone(),
                    profile_id: m.profile_id.clone(),
                })
                .unwrap_or(OverlayAction::None),
            Overlay::Model {
                provider_selected,
                model_selected,
                model_input,
                providers,
                groups,
                route_picker,
                ..
            } => {
                let pid = providers
                    .get(*provider_selected)
                    .map(|s| s.as_str())
                    .unwrap_or("all");
                let chosen = groups
                    .iter()
                    .filter(|g| {
                        group_matches_provider(pid, g) && group_matches_input(model_input, g)
                    })
                    .nth(*model_selected);
                if !model_input.trim().is_empty()
                    && !chosen.is_some_and(|g| {
                        g.routes
                            .iter()
                            .any(|m| m.model.eq_ignore_ascii_case(model_input.trim()))
                    })
                {
                    OverlayAction::RunCommand(format!("/model {}", model_input.trim()))
                } else if let Some(g) = chosen {
                    match g.routes.as_slice() {
                        [] => OverlayAction::None,
                        [single] => OverlayAction::SelectModel {
                            provider: single.provider.clone(),
                            model: single.model.clone(),
                            profile_id: single.profile_id.clone(),
                        },
                        many => {
                            *route_picker = Some(RoutePickerState {
                                model_id: g.model_id.clone(),
                                selected: 0,
                                routes: many.to_vec(),
                            });
                            OverlayAction::None
                        }
                    }
                } else {
                    OverlayAction::None
                }
            }
            Overlay::ConnectApiKey {
                profile_id,
                key_input,
                ..
            } => {
                if key_input.trim().is_empty() {
                    OverlayAction::None
                } else {
                    OverlayAction::ConnectSubmitKey {
                        profile_id: profile_id.clone(),
                        api_key: key_input.clone(),
                    }
                }
            }
            Overlay::ConnectOauth { profile_id, .. } => OverlayAction::ConnectCompleteOauth {
                profile_id: profile_id.clone(),
            },
            Overlay::ConnectPicker {
                selected, items, ..
            } => {
                if let Some(it) = items.get(*selected) {
                    OverlayAction::ConnectPickProfile {
                        profile_id: it.id.clone(),
                    }
                } else {
                    OverlayAction::None
                }
            }
            Overlay::ResumePicker {
                selected, items, ..
            } => {
                if let Some(item) = items.get(*selected) {
                    OverlayAction::RunCommand(format!("/resume {}", item.id))
                } else {
                    OverlayAction::None
                }
            }
            Overlay::QuickOpen { selected, hits, .. } => hits
                .get(*selected)
                .map(|item| OverlayAction::QuickOpenFile {
                    path: item.path.clone(),
                })
                .unwrap_or(OverlayAction::None),
            Overlay::Theme {
                selected, items, ..
            } => items
                .get(*selected)
                .copied()
                .map(OverlayAction::SelectTheme)
                .unwrap_or(OverlayAction::None),
            Overlay::FileExplorer {
                selected, items, ..
            } => {
                if let Some(item) = items.get(*selected) {
                    OverlayAction::FilePick {
                        path: item.path.clone(),
                        is_dir: item.is_dir,
                    }
                } else {
                    OverlayAction::None
                }
            }
            Overlay::FileViewer { .. } => OverlayAction::None,
            Overlay::Hitl { approval, .. } => match approval.focused_action {
                ApprovalFocusedAction::AllowOnce => OverlayAction::HitlApprove,
                ApprovalFocusedAction::RememberDirect if approval.remember_eligible => {
                    OverlayAction::HitlApproveSession
                }
                ApprovalFocusedAction::RememberDirect => OverlayAction::None,
                ApprovalFocusedAction::Deny => OverlayAction::HitlDeny,
            },
        },
        // Use-env must NOT steal literal e/E from pasted API keys (keys almost always
        // contain those letters). Only when the field is still empty + env is available.
        Key::Char('e') | Key::Char('E')
            if matches!(
                overlay,
                Overlay::ConnectApiKey {
                    env_hint: Some(_),
                    key_input,
                    ..
                } if key_input.is_empty()
            ) =>
        {
            if let Overlay::ConnectApiKey { profile_id, .. } = overlay {
                OverlayAction::ConnectUseEnv {
                    profile_id: profile_id.clone(),
                }
            } else {
                OverlayAction::None
            }
        }
        Key::Char(c) if matches!(overlay, Overlay::ConnectApiKey { .. }) => {
            if let Overlay::ConnectApiKey { key_input, .. } = overlay {
                // Ignore whitespace/newlines from sloppy paste; keep other printable chars.
                if !c.is_control() && !c.is_whitespace() {
                    key_input.push(c);
                }
            }
            OverlayAction::None
        }
        Key::Paste(ref data) if matches!(overlay, Overlay::ConnectApiKey { .. }) => {
            if let Overlay::ConnectApiKey { key_input, .. } = overlay {
                for c in data.chars() {
                    if !c.is_control() && !c.is_whitespace() {
                        key_input.push(c);
                    }
                }
            }
            OverlayAction::None
        }
        Key::Char(c)
            if matches!(
                overlay,
                Overlay::Model {
                    route_picker: None,
                    ..
                }
            ) =>
        {
            if let Overlay::Model {
                model_input,
                model_selected,
                ..
            } = overlay
            {
                if !c.is_control() && !c.is_whitespace() {
                    model_input.push(c);
                    *model_selected = 0;
                }
            }
            OverlayAction::None
        }
        Key::Char(c) if matches!(overlay, Overlay::QuickOpen { .. }) => {
            if let Overlay::QuickOpen {
                query, selected, ..
            } = overlay
            {
                if !c.is_control() {
                    query.push(c);
                    *selected = 0;
                }
            }
            OverlayAction::None
        }
        Key::Paste(ref data)
            if matches!(
                overlay,
                Overlay::Model {
                    route_picker: None,
                    ..
                }
            ) =>
        {
            if let Overlay::Model {
                model_input,
                model_selected,
                ..
            } = overlay
            {
                for c in data.chars() {
                    if !c.is_control() && !c.is_whitespace() {
                        model_input.push(c);
                    }
                }
                *model_selected = 0;
            }
            OverlayAction::None
        }
        Key::Paste(ref data) if matches!(overlay, Overlay::QuickOpen { .. }) => {
            if let Overlay::QuickOpen {
                query, selected, ..
            } = overlay
            {
                for c in data.chars() {
                    if !c.is_control() {
                        query.push(c);
                    }
                }
                *selected = 0;
            }
            OverlayAction::None
        }
        Key::Backspace
            if matches!(
                overlay,
                Overlay::Model {
                    route_picker: None,
                    ..
                }
            ) =>
        {
            if let Overlay::Model {
                model_input,
                model_selected,
                ..
            } = overlay
            {
                model_input.pop();
                *model_selected = 0;
            }
            OverlayAction::None
        }
        Key::Backspace if matches!(overlay, Overlay::QuickOpen { .. }) => {
            if let Overlay::QuickOpen {
                query, selected, ..
            } = overlay
            {
                query.pop();
                *selected = 0;
            }
            OverlayAction::None
        }
        Key::Backspace if matches!(overlay, Overlay::ConnectApiKey { .. }) => {
            if let Overlay::ConnectApiKey { key_input, .. } = overlay {
                key_input.pop();
            }
            OverlayAction::None
        }
        Key::Backspace
            if matches!(
                overlay,
                Overlay::FileExplorer { .. } | Overlay::FileViewer { .. }
            ) =>
        {
            match overlay {
                Overlay::FileExplorer { cwd, .. } => parent_dir(cwd),
                Overlay::FileViewer { path, .. } => parent_dir(path),
                _ => None,
            }
            .map(|path| OverlayAction::FilePick { path, is_dir: true })
            .unwrap_or(OverlayAction::None)
        }
        Key::Tab
            if matches!(
                overlay,
                Overlay::Model {
                    route_picker: None,
                    ..
                }
            ) =>
        {
            if let Overlay::Model {
                provider_selected,
                model_selected,
                model_input,
                providers,
                groups,
                ..
            } = overlay
            {
                let pid = providers
                    .get(*provider_selected)
                    .map(|s| s.as_str())
                    .unwrap_or("all");
                if let Some(g) = groups
                    .iter_mut()
                    .filter(|g| {
                        group_matches_provider(pid, g) && group_matches_input(model_input, g)
                    })
                    .nth(*model_selected)
                {
                    g.expanded = !g.expanded;
                }
            }
            OverlayAction::None
        }
        Key::Tab if matches!(overlay, Overlay::Hitl { .. }) => {
            if let Overlay::Hitl { approval, .. } = overlay {
                approval.focus_next(1);
            }
            OverlayAction::None
        }
        Key::BackTab if matches!(overlay, Overlay::Hitl { .. }) => {
            if let Overlay::Hitl { approval, .. } = overlay {
                approval.focus_next(-1);
            }
            OverlayAction::None
        }
        Key::Char('a') | Key::Char('A') if matches!(overlay, Overlay::Hitl { .. }) => {
            OverlayAction::HitlApprove
        }
        Key::Char('s') | Key::Char('S') if matches!(overlay, Overlay::Hitl { .. }) => {
            if let Overlay::Hitl { approval, .. } = overlay {
                if approval.remember_eligible {
                    OverlayAction::HitlApproveSession
                } else {
                    OverlayAction::None
                }
            } else {
                OverlayAction::None
            }
        }
        Key::Char('d') | Key::Char('D') if matches!(overlay, Overlay::Hitl { .. }) => {
            OverlayAction::HitlDeny
        }
        Key::Char('v') | Key::Char('V') if matches!(overlay, Overlay::Hitl { .. }) => {
            if let Overlay::Hitl {
                ref mut expanded, ..
            } = overlay
            {
                *expanded = !*expanded;
            }
            OverlayAction::None
        }
        Key::Char('y') | Key::Char('Y') if matches!(overlay, Overlay::TurnLimit { .. }) => {
            OverlayAction::ContinueTurns
        }
        Key::Char('n') | Key::Char('N') if matches!(overlay, Overlay::TurnLimit { .. }) => {
            OverlayAction::StopTurns
        }
        _ => OverlayAction::None,
    }
}

fn hitl_args(args: &serde_json::Value) -> String {
    let value = args
        .get("command")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| serde_json::to_string(args).unwrap_or_else(|_| "{}".into()));
    value.chars().take(300).collect()
}

#[cfg(test)]
fn hitl_risk_summary(tool: &str, args: &serde_json::Value) -> &'static str {
    // Deterministic consequence summary based on tool and argument metadata.
    // Keep these concise; the expanded policy details contain the full reason.
    match tool {
        // File-write tools.
        "write" => "This writes or modifies file contents.",
        "edit" | "edit_file" | "publish" => "This edits or patches a file.",
        "append" => "This appends content to a file.",

        // Shell / command execution.
        "bash" | "sh" | "cmd" | "powershell" | "shell" | "exec" => {
            "This runs a shell command with your permissions."
        }

        // Network / remote.
        "http" | "fetch" | "curl" | "wget" => "This makes an external network request.",
        "ssh" | "scp" | "rsync" => "This connects to a remote host.",
        "git" | "go_git"
            if args
                .get("command")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.starts_with("push")) =>
        {
            "This pushes changes to a remote repository."
        }
        "git" | "go_git" => "This runs a Git command in the repository.",
        "fork" | "clone" => "This creates a copy of a remote repository.",

        // Package / system mutation.
        "install" | "pip_install" | "npm_install" | "cargo_install" | "brew" | "apt"
        | "apt-get" | "apk" | "pacman" => "This installs or modifies system packages.",
        "rm" | "remove" | "del" | "delete" | "unlink" => {
            "This permanently removes files or directories."
        }
        "mv" | "rename" | "move" => "This moves or renames files.",

        // Database.
        "sql" | "psql" | "mysql" | "sqlite" => "This executes a database query.",

        // Unknown.
        _ => "This command may modify your workspace or cause external side effects.",
    }
}

#[cfg(test)]
fn hitl_command(tool: &str, args: &serde_json::Value) -> String {
    let raw = hitl_args(args);
    if tool == "bash" {
        format!("$ {raw}")
    } else if tool == "write" || tool == "edit" {
        if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
            format!("{tool}  {path}")
        } else {
            format!("{tool}  {raw}")
        }
    } else {
        raw
    }
}

fn approval_mode_for_tool(tool: &str) -> ApprovalExecutionMode {
    match tool {
        "bash" | "sh" | "cmd" | "powershell" | "shell" | "exec" => ApprovalExecutionMode::Shell,
        _ => ApprovalExecutionMode::Direct,
    }
}

fn approval_executable_for_tool(tool: &str) -> String {
    match tool {
        "bash" | "sh" | "cmd" | "powershell" | "shell" | "exec" => tool.to_owned(),
        "git" | "go_git" => "git".into(),
        other => other.to_owned(),
    }
}

fn approval_argument_vector(tool: &str, args: &serde_json::Value) -> Vec<String> {
    if approval_mode_for_tool(tool) == ApprovalExecutionMode::Shell {
        return args
            .get("command")
            .and_then(|value| value.as_str())
            .map(|command| vec![command.to_owned()])
            .unwrap_or_default();
    }
    if matches!(tool, "git" | "go_git") {
        let mut argv = Vec::new();
        if let Some(subcommand) = args.get("subcommand").and_then(|value| value.as_str()) {
            argv.push(subcommand.to_owned());
        }
        if let Some(extra) = args.get("args").and_then(|value| value.as_array()) {
            argv.extend(
                extra
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_owned)),
            );
        }
        if !argv.is_empty() {
            return argv;
        }
    }
    vec![serde_json::to_string(args).unwrap_or_else(|_| "{}".into())]
}

fn approval_environment_delta(args: &serde_json::Value) -> String {
    args.get("environment_delta")
        .or_else(|| args.get("env_delta"))
        .or_else(|| args.get("env"))
        .map(|value| serde_json::to_string(value).unwrap_or_else(|_| "[unavailable]".into()))
        .unwrap_or_else(|| "inherited".into())
        .chars()
        .take(300)
        .collect()
}

fn approval_working_directory(args: &serde_json::Value, fallback: String) -> String {
    args.get("working_directory")
        .or_else(|| args.get("cwd"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or(fallback)
}

fn contains_redacted_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(value) => value == "[REDACTED]",
        serde_json::Value::Array(values) => values.iter().any(contains_redacted_value),
        serde_json::Value::Object(values) => values.values().any(contains_redacted_value),
        _ => false,
    }
}

fn action_label(action: ApprovalFocusedAction) -> &'static str {
    match action {
        ApprovalFocusedAction::AllowOnce => "Allow once",
        ApprovalFocusedAction::RememberDirect => "Remember exact Direct",
        ApprovalFocusedAction::Deny => "Deny",
    }
}

fn action_span(action: ApprovalFocusedAction, focused: ApprovalFocusedAction) -> Span<'static> {
    let label = format!("[{}]", action_label(action));
    let style = if action == focused {
        theme::focused_selection_style()
    } else if action == ApprovalFocusedAction::Deny {
        theme::danger()
    } else {
        theme::text()
    };
    Span::styled(label, style)
}

fn approval_lines(payload: &HitlPayload, approval: &ApprovalOverlayState) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled("Approval required", theme::warn())),
        Line::from(""),
        Line::from(vec![
            Span::styled("Mode: ", theme::muted()),
            Span::styled(approval.mode.label().to_owned(), theme::text()),
        ]),
    ];
    match approval.mode {
        ApprovalExecutionMode::Direct => {
            lines.push(Line::from(vec![
                Span::styled("Executable: ", theme::muted()),
                Span::styled(approval.executable_or_shell.clone(), theme::text()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Arguments: ", theme::muted()),
                Span::styled(format!("{:?}", approval.arguments), theme::text()),
            ]));
        }
        ApprovalExecutionMode::Shell => {
            lines.push(Line::from(vec![
                Span::styled("Shell: ", theme::muted()),
                Span::styled(approval.executable_or_shell.clone(), theme::text()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Command: ", theme::muted()),
                Span::styled(
                    approval.shell_command.clone().unwrap_or_default(),
                    theme::text(),
                ),
            ]));
        }
    }
    lines.extend([
        Line::from(vec![
            Span::styled("Working directory: ", theme::muted()),
            Span::styled(approval.working_directory.clone(), theme::text()),
        ]),
        Line::from(vec![
            Span::styled("Environment delta: ", theme::muted()),
            Span::styled(approval.environment_delta.clone(), theme::text()),
        ]),
        Line::from(vec![
            Span::styled("Source: ", theme::muted()),
            Span::styled(approval.source.clone(), theme::text()),
        ]),
        Line::from(vec![
            Span::styled("Reason: ", theme::muted()),
            Span::styled(payload.reason.clone(), theme::text()),
        ]),
        Line::from(""),
        Line::from(vec![
            action_span(ApprovalFocusedAction::AllowOnce, approval.focused_action),
            Span::raw("  "),
            action_span(ApprovalFocusedAction::Deny, approval.focused_action),
        ]),
    ]);
    if approval.remember_eligible {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            action_span(
                ApprovalFocusedAction::RememberDirect,
                approval.focused_action,
            ),
            Span::raw(" / s"),
        ]));
        lines.push(Line::from(
            "Remember this exact Direct invocation in this workspace",
        ));
        lines.push(Line::from("for the remainder of this Forge session."));
    } else if approval.mode == ApprovalExecutionMode::Shell {
        lines.push(Line::from(""));
        lines.push(Line::from("Shell-mode approvals are one-time only."));
    }
    lines.push(Line::from(""));
    lines.push(Line::from("Enter/a allow once · d/Esc deny · Tab move"));
    lines.push(Line::from("v view details"));
    lines
}

fn approval_detail_lines(
    payload: &HitlPayload,
    approval: &ApprovalOverlayState,
) -> Vec<Line<'static>> {
    vec![
        Line::from(""),
        Line::from(Span::styled("details", theme::warn())),
        Line::from(vec![
            Span::styled("Environment delta: ", theme::muted()),
            Span::styled(approval.environment_delta.clone(), theme::text()),
        ]),
        Line::from(vec![
            Span::styled("Source: ", theme::muted()),
            Span::styled(approval.source.clone(), theme::text()),
        ]),
        Line::from(vec![
            Span::styled("Tool: ", theme::muted()),
            Span::styled(payload.tool.clone(), theme::text()),
        ]),
        Line::from(vec![
            Span::styled("Args: ", theme::muted()),
            Span::styled(hitl_args(&payload.args_redacted), theme::text()),
        ]),
        Line::from(vec![
            Span::styled("Policy: ", theme::muted()),
            Span::styled(payload.reason.clone(), theme::text()),
        ]),
        Line::from("Secrets are not shown."),
    ]
}

fn parent_dir(path: &str) -> Option<String> {
    Path::new(path)
        .parent()
        .map(|path| path.display().to_string())
}

/// Minimal key enum for testable overlay handling (mapped from crossterm in app).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Esc,
    Enter,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Backspace,
    Char(char),
    /// Bracketed-paste payload (full string at once).
    Paste(String),
    Other,
}

pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup[1])[1]
}

/// Build styled spans for a Quick Open path, highlighting query match ranges.
fn quick_open_path_spans(
    path: &str,
    match_ranges: &[(u32, u32)],
    selected: bool,
) -> Vec<Span<'static>> {
    let base = if selected {
        theme::focused_selection_style()
    } else {
        theme::text()
    };
    if match_ranges.is_empty() {
        return vec![Span::styled(path.to_owned(), base)];
    }

    let bytes = path.as_bytes();
    let mut ranges: Vec<(usize, usize)> = match_ranges
        .iter()
        .map(|&(start, end)| (start as usize, end as usize))
        .filter(|&(start, end)| start < end && start < bytes.len())
        .collect();
    ranges.sort_by_key(|range| range.0);

    let match_style = if selected {
        theme::search_match().add_modifier(Modifier::BOLD)
    } else {
        theme::search_match()
    };

    let mut spans = Vec::new();
    let mut cursor = 0usize;
    for (start, end) in ranges {
        let start = start.min(bytes.len());
        let end = end.min(bytes.len()).max(start);
        if start > cursor {
            spans.push(Span::styled(
                String::from_utf8_lossy(&bytes[cursor..start]).into_owned(),
                base,
            ));
        }
        if end > start {
            spans.push(Span::styled(
                String::from_utf8_lossy(&bytes[start..end]).into_owned(),
                match_style,
            ));
        }
        cursor = cursor.max(end);
    }
    if cursor < bytes.len() {
        spans.push(Span::styled(
            String::from_utf8_lossy(&bytes[cursor..]).into_owned(),
            base,
        ));
    }
    spans
}

/// Centred panel that remains usable on small terminals without becoming a
/// nearly full-width dashboard on large ones.
fn centered_capped_rect(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = area.width.saturating_sub(4).min(max_width).max(1);
    let height = area.height.saturating_sub(4).min(max_height).max(1);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

pub struct OverlayWidget<'a> {
    pub overlay: &'a Overlay,
}

impl Widget for OverlayWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if !matches!(self.overlay, Overlay::Hitl { .. }) {
            theme::fill(area, buf, theme::canvas());
        }
        match self.overlay {
            Overlay::Help => {
                let r = centered_rect(64, 58, area);
                Paragraph::new(
                    "Forge is an AI coding agent for your terminal.\n\nStart typing and press Enter.\n\nShortcuts\n• /       Commands\n• Ctrl+B  Toggle inspector\n• Tab / Shift+Tab  Focus visible blocks\n• Ctrl+P  Quick Open files\n• Ctrl+`  Toggle bottom panel\n• Alt+1-4 Open bottom-panel tabs\n• ⇧← / ⇧→  Switch tab in the active block\n• Enter/i Interact\n• Tab     Complete (Chat composer)\n• ↑↓      Navigate local list or input\n• Esc     Leave one interaction level\n• ?       Help\n\nEditor (when a file is open)\n• Ctrl+F  Search file\n• Ctrl+G  Jump to line\n• G / r   Editor navigation and refresh\n• Esc     Return to workspace\n\nForge asks before sensitive actions and automatically saves your session.\n\nPress Enter to get started.",
                )
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
            Overlay::Effort {
                model,
                selected,
                current,
                default,
                items,
            } => {
                let r = centered_capped_rect(area, 54, 14);
                let list_items: Vec<ListItem> = items
                    .iter()
                    .enumerate()
                    .map(|(index, effort)| {
                        let marker = if index == *selected { "▶ " } else { "  " };
                        let current = if effort == current { " current" } else { "" };
                        let default_label = if effort == default {
                            " (provider default)"
                        } else {
                            ""
                        };
                        let style = if index == *selected {
                            theme::focused_selection_style()
                        } else {
                            theme::text()
                        };
                        ListItem::new(Span::styled(
                            format!("{marker}{}{current}{default_label}", effort.label()),
                            style,
                        ))
                    })
                    .collect();
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::border())
                    .style(theme::panel())
                    .title(Span::styled(
                        " Reasoning effort · ↑↓ select · Enter use ",
                        theme::brand(),
                    ));
                let inner = block.inner(r);
                block.render(r, buf);
                let regions = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(1),
                        Constraint::Min(1),
                        Constraint::Length(1),
                    ])
                    .split(inner);
                Paragraph::new(format!("Model: {model}"))
                    .style(theme::muted())
                    .render(regions[0], buf);
                List::new(list_items).render(regions[1], buf);
                Paragraph::new("Esc close")
                    .style(theme::dim())
                    .render(regions[2], buf);
            }
            Overlay::StatusReport { title, lines } => {
                let r = centered_capped_rect(area, 74, 30);
                Paragraph::new(lines.join("\n"))
                    .wrap(ratatui::widgets::Wrap { trim: true })
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::warn())
                            .style(theme::panel())
                            .title(Span::styled(format!(" {title} "), theme::warn())),
                    )
                    .render(r, buf);
            }
            Overlay::TurnLimit { turns } => {
                let r = centered_rect(52, 24, area);
                let body = format!(
                    "The agent used {turns} model steps and still has work to do.\n\nContinue for another {turns} steps?\n\n[y/Enter] Continue    [n/Esc] Stop"
                );
                Paragraph::new(body)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::warn())
                            .title(Span::styled(
                                " Turn limit reached ",
                                theme::warn().add_modifier(Modifier::BOLD),
                            )),
                    )
                    .render(r, buf);
            }
            Overlay::Hitl {
                payload,
                approval,
                expanded,
            } => {
                let r = centered_capped_rect(area, 78, if *expanded { 30 } else { 22 });
                let mut lines = approval_lines(payload, approval);
                if *expanded {
                    lines.extend(approval_detail_lines(payload, approval));
                }

                Paragraph::new(lines)
                    .wrap(ratatui::widgets::Wrap { trim: true })
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::warn())
                            .style(theme::panel())
                            .title(Span::styled(
                                " Approval required ",
                                theme::warn().add_modifier(Modifier::BOLD),
                            )),
                    )
                    .render(r, buf);
            }
            Overlay::Model {
                provider_selected,
                model_selected,
                model_input,
                current_model,
                providers,
                groups,
                route_picker,
                current_effort,
            } => {
                if let Some(rp) = route_picker {
                    let r = centered_capped_rect(area, 60, 12);
                    let list_items: Vec<ListItem> = rp
                        .routes
                        .iter()
                        .enumerate()
                        .map(|(i, route)| {
                            let marker = if i == rp.selected { "▶ " } else { "  " };
                            let style = if i == rp.selected {
                                theme::focused_selection_style()
                            } else {
                                theme::text()
                            };
                            let label = route.profile_id.as_deref().unwrap_or_else(|| {
                                route.model.split('/').next().unwrap_or("provider")
                            });
                            ListItem::new(Span::styled(format!("{marker}{label}"), style))
                        })
                        .collect();
                    let title = format!(" {} · Select Route · ↑↓ select · Enter use ", rp.model_id);
                    let block = Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::border())
                        .style(theme::panel())
                        .title(Span::styled(title, theme::brand()));
                    let inner = block.inner(r);
                    block.render(r, buf);
                    let regions = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Min(1), Constraint::Length(1)])
                        .split(inner);
                    List::new(list_items).render(regions[0], buf);
                    Paragraph::new("Esc back")
                        .style(theme::dim())
                        .render(regions[1], buf);
                    return;
                }
                let r = centered_capped_rect(area, 88, 20);
                let pid = providers
                    .get(*provider_selected)
                    .map(|s| s.as_str())
                    .unwrap_or("all");
                let filtered: Vec<&ModelGroup> = groups
                    .iter()
                    .filter(|g| {
                        group_matches_provider(pid, g) && group_matches_input(model_input, g)
                    })
                    .collect();
                let total = filtered.len();
                let visible = r.height.saturating_sub(6).max(1) as usize;
                let start = if *model_selected < visible {
                    0
                } else if *model_selected + 1 > visible {
                    (*model_selected).saturating_add(1).saturating_sub(visible)
                } else {
                    0
                };
                let end = (start + visible).min(filtered.len());
                let window = &filtered[start..end];
                let list_items: Vec<ListItem> = if window.is_empty() {
                    vec![ListItem::new(Span::styled(
                        if groups.is_empty() {
                            "No models are available. Connect a provider or start a local model runtime."
                        } else {
                            "No models match this provider or filter."
                        },
                        theme::muted(),
                    ))]
                } else {
                    window
                        .iter()
                        .enumerate()
                        .flat_map(|(i, g)| {
                            let idx = start + i;
                            let marker = if idx == *model_selected { "▶ " } else { "  " };
                            let style = if idx == *model_selected {
                                theme::focused_selection_style()
                            } else {
                                theme::text()
                            };
                            let is_current = g.routes.iter().any(|m| m.model == *current_model);
                            let route_count = g.routes.len();
                            let tag = if route_count > 1 {
                                format!("{route_count} routes")
                            } else {
                                let m = &g.routes[0];
                                let provider =
                                    m.model.split_once('/').map(|(p, _)| p).unwrap_or_else(|| {
                                        m.profile_id.as_deref().unwrap_or("model")
                                    });
                                if is_current {
                                    "current".to_string()
                                } else if m.source == forge_connect::CatalogSource::Registry {
                                    "known".to_string()
                                } else if provider == "ollama" || provider.contains("local") {
                                    "local".to_string()
                                } else {
                                    "cloud".to_string()
                                }
                            };
                            let label = if route_count > 1 {
                                g.model_id.clone()
                            } else {
                                let m = &g.routes[0];
                                let (provider, model) = m.model.split_once('/').unwrap_or((
                                    m.profile_id.as_deref().unwrap_or("model"),
                                    &m.model,
                                ));
                                format!("{provider} / {model}")
                            };
                            let mut row = format!("{marker}{label}");
                            let tag_display = if is_current && route_count > 1 {
                                format!("current · {tag}")
                            } else {
                                tag
                            };
                            let target = (r.width.saturating_sub(5) as usize)
                                .saturating_sub(tag_display.chars().count());
                            while row.chars().count() < target {
                                row.push(' ');
                            }
                            row.push_str(&tag_display);
                            let mut rows = vec![ListItem::new(Span::styled(row, style))];
                            if g.expanded {
                                rows.extend(g.routes.iter().map(|route| {
                                    let provider_label =
                                        route.profile_id.as_deref().unwrap_or_else(|| {
                                            route.model.split('/').next().unwrap_or("provider")
                                        });
                                    ListItem::new(Span::styled(
                                        format!("      via {provider_label}"),
                                        theme::dim(),
                                    ))
                                }));
                            }
                            rows
                        })
                        .collect()
                };
                let prov_hint = if providers.len() > 1 {
                    " · ←/→ provider"
                } else {
                    ""
                };
                let page = if total == 0 {
                    "0/0".into()
                } else {
                    format!("{}/{}", (*model_selected + 1).min(total), total)
                };
                let title = format!(
                    " Models · {pid} · {page}{prov_hint} · ↑↓ select · Enter use · Tab routes "
                );
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::border())
                    .style(theme::panel())
                    .title(Span::styled(title, theme::brand()));
                let inner = block.inner(r);
                block.render(r, buf);
                let regions = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(1),
                        Constraint::Length(2),
                        Constraint::Min(1),
                        Constraint::Length(1),
                        Constraint::Length(1),
                    ])
                    .split(inner);
                Paragraph::new(format!("Current: {current_model}"))
                    .style(theme::muted())
                    .render(regions[0], buf);
                Paragraph::new(format!("/model {model_input}█"))
                    .style(theme::text())
                    .render(regions[1], buf);
                List::new(list_items).render(regions[2], buf);
                Paragraph::new(format!("Effort: {}", current_effort.label()))
                    .style(theme::muted())
                    .render(regions[3], buf);
                Paragraph::new("known = public registry · Esc close")
                    .style(theme::dim())
                    .render(regions[4], buf);
            }
            Overlay::ConnectApiKey {
                title,
                auth_url,
                key_input,
                env_hint,
                error,
                ..
            } => {
                let r = centered_rect(66, 42, area);
                let masked: String = "•".repeat(key_input.chars().count());
                let url = auth_url.as_deref().unwrap_or("(see docs)");
                let n = key_input.chars().count();
                let env_line = env_hint
                    .as_ref()
                    .map(|h| format!("\n[e] Use {h}"))
                    .unwrap_or_default();
                let error_line = error
                    .as_ref()
                    .map(|e| format!("\n\nCould not connect: {e}"))
                    .unwrap_or_default();
                let body = format!(
                    "{title}\n\nCreate or manage keys:\n{url}\n\nPaste API key:\n{masked}█  ({n} chars){error_line}\n\nEnter connect · Esc back{env_line}"
                );
                Paragraph::new(body)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::border())
                            .style(theme::panel())
                            .title(Span::styled(
                                " Connect with API key ",
                                theme::brand().add_modifier(Modifier::BOLD),
                            )),
                    )
                    .render(r, buf);
            }
            Overlay::ConnectOauth {
                title,
                instructions,
                ..
            } => {
                let r = centered_rect(70, 50, area);
                let body = format!(
                    "{title}\n\n{instructions}\n\nWaiting for sign-in…\nEnter check now · Esc back"
                );
                Paragraph::new(body)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::border())
                            .style(theme::panel())
                            .title(Span::styled(" Sign in ", theme::brand())),
                    )
                    .render(r, buf);
            }
            Overlay::QuickOpen {
                query,
                selected,
                hits,
                error,
            } => {
                let r = centered_capped_rect(area, 88, 20);
                let visible = r.height.saturating_sub(6).max(1) as usize;
                let start = if *selected < visible {
                    0
                } else if *selected + 1 > visible {
                    (*selected).saturating_add(1).saturating_sub(visible)
                } else {
                    0
                };
                let end = (start + visible).min(hits.len());
                let window = &hits[start..end];
                let list_items: Vec<ListItem> = if let Some(message) = error {
                    vec![ListItem::new(Span::styled(message, theme::warn()))]
                } else if window.is_empty() {
                    vec![ListItem::new(Span::styled(
                        if query.trim().is_empty() {
                            "Type to search files…"
                        } else {
                            "No matching files"
                        },
                        theme::muted(),
                    ))]
                } else {
                    window
                        .iter()
                        .enumerate()
                        .map(|(i, hit)| {
                            let idx = start + i;
                            let marker = if idx == *selected { "▶ " } else { "  " };
                            let marker_style = if idx == *selected {
                                theme::focused_selection_style()
                            } else {
                                theme::text()
                            };
                            let mut spans = vec![Span::styled(marker.to_string(), marker_style)];
                            spans.extend(quick_open_path_spans(
                                &hit.path,
                                &hit.match_ranges,
                                idx == *selected,
                            ));
                            ListItem::new(Line::from(spans))
                        })
                        .collect()
                };
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(3), Constraint::Min(3)])
                    .split(r);
                Paragraph::new(format!("{query}█"))
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::border())
                            .style(theme::panel())
                            .title(Span::styled(
                                " Quick Open · ↑↓ Enter · Esc cancel ",
                                theme::brand(),
                            )),
                    )
                    .render(chunks[0], buf);
                List::new(list_items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::border())
                            .style(theme::panel()),
                    )
                    .render(chunks[1], buf);
            }
            Overlay::ResumePicker { selected, items } => {
                let r = centered_rect(70, 48, area);
                let visible = r.height.saturating_sub(2).max(1) as usize;
                let start = selected
                    .saturating_add(1)
                    .saturating_sub(visible)
                    .min(items.len().saturating_sub(visible));
                let list_items: Vec<ListItem> = items
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(visible)
                    .map(|(index, item)| {
                        let marker = if index == *selected { "▶ " } else { "  " };
                        let style = if index == *selected {
                            theme::selected_row()
                        } else {
                            theme::text()
                        };
                        let row = format!("{marker}{}  ·  {}", item.id, item.modified);
                        ListItem::new(Span::styled(row, style))
                    })
                    .collect();
                List::new(list_items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::border())
                            .style(theme::panel())
                            .title(Span::styled(
                                " Resume a session · ↑↓ Enter · Esc cancel ",
                                theme::brand(),
                            )),
                    )
                    .render(r, buf);
            }
            Overlay::Theme {
                selected,
                current,
                items,
            } => {
                let r = centered_rect(54, 16, area);
                let list_items: Vec<ListItem> = items
                    .iter()
                    .enumerate()
                    .map(|(index, choice)| {
                        let marker = if index == *selected { "▶ " } else { "  " };
                        let current = if *choice == *current {
                            " · current"
                        } else {
                            ""
                        };
                        let style = if index == *selected {
                            theme::focused_selection_style()
                        } else {
                            theme::text()
                        };
                        ListItem::new(Span::styled(
                            format!("{marker}{} ({}){current}", choice.title(), choice.label()),
                            style,
                        ))
                    })
                    .collect();
                List::new(list_items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::border())
                            .style(theme::panel())
                            .title(Span::styled(
                                " Theme · ↑↓ select · Enter apply ",
                                theme::brand(),
                            )),
                    )
                    .render(r, buf);
            }
            Overlay::FileExplorer {
                cwd,
                selected,
                items,
                error,
            } => {
                let r = centered_rect(76, 64, area);
                let visible = r.height.saturating_sub(4).max(1) as usize;
                let start = selected
                    .saturating_add(1)
                    .saturating_sub(visible)
                    .min(items.len().saturating_sub(visible));
                let list_items: Vec<ListItem> = items
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(visible)
                    .map(|(index, item)| {
                        let marker = if index == *selected { "▶ " } else { "  " };
                        let style = if index == *selected {
                            theme::selected_row()
                        } else {
                            theme::text()
                        };
                        let kind = if item.is_dir { "📁" } else { "  " };
                        ListItem::new(Span::styled(format!("{marker}{kind} {}", item.name), style))
                    })
                    .collect();
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::border())
                    .style(theme::panel())
                    .title(Span::styled(
                        " File explorer · readonly · ↑↓ Enter · ←/Backspace up · Esc close ",
                        theme::brand(),
                    ));
                let inner = block.inner(r);
                block.render(r, buf);
                let regions = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(1),
                        Constraint::Min(1),
                        Constraint::Length(1),
                    ])
                    .split(inner);
                Paragraph::new(cwd.as_str())
                    .style(theme::muted())
                    .render(regions[0], buf);
                List::new(list_items).render(regions[1], buf);
                let status = error
                    .as_deref()
                    .unwrap_or("Enter opens directories/files · ←/Backspace moves up");
                Paragraph::new(status)
                    .style(theme::muted())
                    .render(regions[2], buf);
            }
            Overlay::FileViewer {
                path,
                lines,
                scroll,
            } => {
                let r = centered_rect(86, 78, area);
                let visible = r.height.saturating_sub(2).max(1) as usize;
                let width = r.width.saturating_sub(2) as usize;
                let body = lines
                    .iter()
                    .enumerate()
                    .skip(*scroll)
                    .take(visible)
                    .map(|(index, line)| {
                        let mut row = format!("{:>4} │ {}", index + 1, line);
                        row.truncate(width);
                        row
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let title = format!(
                    " {} · readonly · {}/{} · ↑↓ scroll · ←/Backspace back · Esc close ",
                    path,
                    (*scroll + 1).min(lines.len().max(1)),
                    lines.len().max(1)
                );
                Paragraph::new(body)
                    .style(theme::text())
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::border())
                            .style(theme::panel())
                            .title(Span::styled(title, theme::brand())),
                    )
                    .render(r, buf);
            }
            Overlay::ConnectPicker { selected, items } => {
                let r = centered_rect(58, 42, area);
                let list_items: Vec<ListItem> = items
                    .iter()
                    .enumerate()
                    .map(|(i, it)| {
                        let marker = if i == *selected { "▶ " } else { "  " };
                        let style = if i == *selected {
                            theme::selected_row()
                        } else {
                            theme::text()
                        };
                        let mode = match it.auth_mode.as_str() {
                            "oauth" => "Sign in",
                            "api_key" if it.id == "ollama" => "Local",
                            "api_key" => "API key",
                            other => other,
                        };
                        let state = if it.connected { "✓ connected" } else { mode };
                        let mut row = format!("{marker}{:<30} {state}", it.title);
                        while row.chars().count() < 48 {
                            row.push(' ');
                        }
                        ListItem::new(Span::styled(row, style))
                    })
                    .collect();
                List::new(list_items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::border())
                            .style(theme::panel())
                            .title(Span::styled(
                                " Choose a provider · ↑↓ Enter ",
                                theme::brand(),
                            )),
                    )
                    .render(r, buf);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{parse_slash, SlashCommand};
    use forge_types::HitlPayload;
    use ratatui::widgets::Widget;
    use serde_json::json;

    fn render_text(overlay: &Overlay) -> String {
        let area = Rect::new(0, 0, 100, 48);
        let mut buf = Buffer::empty(area);
        OverlayWidget { overlay }.render(area, &mut buf);
        let mut text = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn quick_open_path_spans_highlight_match_ranges() {
        let spans = quick_open_path_spans("src/main.rs", &[(0, 3), (4, 8)], false);
        assert_eq!(spans.len(), 4);
        assert_eq!(spans[0].content, "src");
        assert_eq!(spans[0].style, theme::search_match());
        assert_eq!(spans[1].content, "/");
        assert_eq!(spans[1].style, theme::text());
        assert_eq!(spans[2].content, "main");
        assert_eq!(spans[2].style, theme::search_match());
        assert_eq!(spans[3].content, ".rs");
        assert_eq!(spans[3].style, theme::text());
    }

    #[test]
    fn quick_open_path_spans_without_ranges_use_base_style() {
        let spans = quick_open_path_spans("README.md", &[], true);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "README.md");
        assert_eq!(spans[0].style, theme::focused_selection_style());
    }

    #[test]
    fn welcome_starts_onboarding() {
        let mut overlay = Overlay::welcome();
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::BeginOnboarding
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Esc),
            OverlayAction::Close
        );
    }

    #[test]
    fn status_report_closes_on_enter() {
        let mut overlay = Overlay::StatusReport {
            title: "Status".into(),
            lines: vec!["status=idle".into()],
        };
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::Close
        );
    }

    #[test]
    fn filter_palette_narrows() {
        let items = filter_palette("app");
        assert!(items.is_empty());
        assert!(!items.iter().any(|i| i.cmd == "/quit"));
    }

    #[test]
    fn default_palette_covers_parseable_commands() {
        let items = default_palette_items();
        // Every bare cmd (no required args) should parse; arg-required ones still listed.
        for it in &items {
            let res = parse_slash(&it.cmd).expect("is slash");
            match it.cmd.as_str() {
                "/resume" => assert_eq!(res.unwrap(), SlashCommand::ResumeList),
                other => assert!(res.is_ok(), "palette cmd {other} should parse: {res:?}"),
            }
        }
        assert!(
            items.len() >= 8,
            "expected full command list, got {}",
            items.len()
        );
    }

    #[test]
    fn hitl_keys() {
        let mut o = Overlay::hitl(HitlPayload {
            call_id: "1".into(),
            tool: "bash".into(),
            args_redacted: json!({"command": "git push"}),
            reason: "policy".into(),
        });
        assert_eq!(
            handle_overlay_key(&mut o, Key::Char('a')),
            OverlayAction::HitlApprove
        );
        assert_eq!(
            handle_overlay_key(&mut o, Key::Char('s')),
            OverlayAction::None
        );
        assert_eq!(
            handle_overlay_key(&mut o, Key::Char('d')),
            OverlayAction::HitlDeny
        );
        assert_eq!(
            handle_overlay_key(&mut o, Key::Esc),
            OverlayAction::HitlDeny
        );
    }

    #[test]
    fn hitl_direct_remember_is_eligible_and_default_focus_allows_once() {
        let mut o = Overlay::hitl(HitlPayload {
            call_id: "1".into(),
            tool: "git".into(),
            args_redacted: json!({"subcommand": "push", "args": ["origin", "main"]}),
            reason: "policy".into(),
        });
        assert_eq!(
            handle_overlay_key(&mut o, Key::Enter),
            OverlayAction::HitlApprove
        );
        assert_eq!(
            handle_overlay_key(&mut o, Key::Char('s')),
            OverlayAction::HitlApproveSession
        );
        handle_overlay_key(&mut o, Key::Tab);
        assert_eq!(
            handle_overlay_key(&mut o, Key::Enter),
            OverlayAction::HitlApproveSession
        );
    }

    #[test]
    fn hitl_toggle_expanded() {
        let mut o = Overlay::hitl(HitlPayload {
            call_id: "1".into(),
            tool: "write".into(),
            args_redacted: json!({"path": "src/main.rs"}),
            reason: "Edit tool requires approval".into(),
        });
        assert!(!matches!(o, Overlay::Hitl { expanded: true, .. }));
        handle_overlay_key(&mut o, Key::Char('v'));
        assert!(matches!(o, Overlay::Hitl { expanded: true, .. }));
        handle_overlay_key(&mut o, Key::Char('v'));
        assert!(matches!(
            o,
            Overlay::Hitl {
                expanded: false,
                ..
            }
        ));
    }

    #[test]
    fn hitl_risk_summary_works_for_known_tools() {
        assert_eq!(
            hitl_risk_summary("bash", &json!({})),
            "This runs a shell command with your permissions."
        );
        assert_eq!(
            hitl_risk_summary("write", &json!({})),
            "This writes or modifies file contents."
        );
        assert_eq!(
            hitl_risk_summary("unknown_tool", &json!({})),
            "This command may modify your workspace or cause external side effects."
        );
    }

    #[test]
    fn hitl_args_prefers_redacted_command() {
        assert_eq!(
            hitl_args(&json!({"command": "git push -u origin feature"})),
            "git push -u origin feature"
        );
        assert_eq!(
            hitl_args(&json!({"path": "src/lib.rs"})),
            r#"{"path":"src/lib.rs"}"#
        );
    }

    #[test]
    fn turn_limit_keys_continue_or_stop() {
        let mut overlay = Overlay::turn_limit(128);
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::ContinueTurns
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Char('y')),
            OverlayAction::ContinueTurns
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Char('n')),
            OverlayAction::StopTurns
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Esc),
            OverlayAction::StopTurns
        );
    }

    #[test]
    fn model_select() {
        let mut o = Overlay::model_open();
        let a = handle_overlay_key(&mut o, Key::Enter);
        match a {
            OverlayAction::SelectModel {
                provider, model, ..
            } => {
                assert_eq!(provider, "native");
                assert!(model.contains('/') || !model.is_empty());
            }
            _ => panic!("expected model select"),
        }
    }

    #[test]
    fn model_accepts_typed_custom_model() {
        let mut overlay = Overlay::model_open();
        for c in "openai/custom-model".chars() {
            assert_eq!(
                handle_overlay_key(&mut overlay, Key::Char(c)),
                OverlayAction::None
            );
        }
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::RunCommand("/model openai/custom-model".into())
        );
    }

    #[test]
    fn model_input_supports_paste_and_backspace() {
        let mut overlay = Overlay::model_open();
        handle_overlay_key(&mut overlay, Key::Paste("anthropic/custom-modelx\n".into()));
        handle_overlay_key(&mut overlay, Key::Backspace);
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::RunCommand("/model anthropic/custom-model".into())
        );
    }

    #[test]
    fn resume_picker_moves_and_runs_selected_session() {
        let mut overlay = Overlay::resume_picker(vec![
            ResumeSessionItem {
                id: "first".into(),
                modified: "newest".into(),
            },
            ResumeSessionItem {
                id: "second".into(),
                modified: "older".into(),
            },
        ]);
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Down),
            OverlayAction::None
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::RunCommand("/resume second".into())
        );
    }

    #[test]
    fn connect_api_key_overlay_requires_key_and_masks() {
        let mut o = Overlay::connect_api_key(
            "opencode_go",
            "OpenCode Go",
            Some("https://opencode.ai/auth".into()),
            None,
        );
        // Enter with empty key does nothing
        assert_eq!(handle_overlay_key(&mut o, Key::Enter), OverlayAction::None);
        handle_overlay_key(&mut o, Key::Char('s'));
        handle_overlay_key(&mut o, Key::Char('e'));
        handle_overlay_key(&mut o, Key::Char('c'));
        let a = handle_overlay_key(&mut o, Key::Enter);
        match a {
            OverlayAction::ConnectSubmitKey {
                profile_id,
                api_key,
            } => {
                assert_eq!(profile_id, "opencode_go");
                assert_eq!(api_key, "sec");
            }
            other => panic!("expected submit key, got {other:?}"),
        }
        if let Overlay::ConnectApiKey { key_input, .. } = &o {
            assert_eq!(key_input, "sec");
        }
    }

    #[test]
    fn connect_api_key_e_types_into_field_even_when_env_hint() {
        // Regression: plain 'e' used to fire ConnectUseEnv and abort paste mid-key.
        let mut o = Overlay::connect_api_key(
            "opencode_go",
            "OpenCode Go",
            Some("https://opencode.ai/auth".into()),
            Some("OPENCODE_API_KEY".into()),
        );
        for c in "sk-test-key-with-e-chars".chars() {
            let a = handle_overlay_key(&mut o, Key::Char(c));
            assert_eq!(a, OverlayAction::None, "char {c:?} must not steal env");
        }
        if let Overlay::ConnectApiKey { key_input, .. } = &o {
            assert_eq!(key_input, "sk-test-key-with-e-chars");
        } else {
            panic!("expected ConnectApiKey");
        }
        let a = handle_overlay_key(&mut o, Key::Enter);
        match a {
            OverlayAction::ConnectSubmitKey { api_key, .. } => {
                assert_eq!(api_key, "sk-test-key-with-e-chars");
            }
            other => panic!("expected submit, got {other:?}"),
        }
    }

    #[test]
    fn connect_api_key_e_uses_env_only_when_field_empty() {
        let mut o = Overlay::connect_api_key(
            "opencode_go",
            "OpenCode Go",
            None,
            Some("OPENCODE_API_KEY".into()),
        );
        let a = handle_overlay_key(&mut o, Key::Char('e'));
        assert_eq!(
            a,
            OverlayAction::ConnectUseEnv {
                profile_id: "opencode_go".into()
            }
        );
    }

    #[test]
    fn connect_api_key_paste_inserts_full_string() {
        let mut o = Overlay::connect_api_key("opencode_go", "OpenCode Go", None, None);
        let pasted = "abc123XYZ-long-api-key-value\n";
        handle_overlay_key(&mut o, Key::Paste(pasted.into()));
        if let Overlay::ConnectApiKey { key_input, .. } = &o {
            assert_eq!(key_input, "abc123XYZ-long-api-key-value");
            assert_eq!(key_input.chars().count(), 28);
        } else {
            panic!("expected ConnectApiKey");
        }
    }

    #[test]
    fn connect_oauth_overlay_enter_completes() {
        let mut o = Overlay::connect_oauth("xai", "xAI Grok", "Visit accounts.x.ai");
        let a = handle_overlay_key(&mut o, Key::Enter);
        assert_eq!(
            a,
            OverlayAction::ConnectCompleteOauth {
                profile_id: "xai".into()
            }
        );
    }

    #[test]
    fn file_explorer_left_moves_to_parent() {
        let mut o = Overlay::file_explorer("/workspace/src", vec![], None);
        let a = handle_overlay_key(&mut o, Key::Left);
        assert_eq!(
            a,
            OverlayAction::FilePick {
                path: "/workspace".into(),
                is_dir: true,
            }
        );
    }

    #[test]
    fn file_viewer_backspace_returns_to_containing_dir() {
        let mut o = Overlay::file_viewer("/workspace/src/lib.rs", "contents");
        let a = handle_overlay_key(&mut o, Key::Backspace);
        assert_eq!(
            a,
            OverlayAction::FilePick {
                path: "/workspace/src".into(),
                is_dir: true,
            }
        );
    }

    #[test]
    fn connect_picker_sorts_providers_alphabetically() {
        let overlay = Overlay::connect_picker(vec![
            ConnectProfileItem {
                id: "xai".into(),
                title: "xAI Grok".into(),
                auth_mode: "oauth".into(),
                auth_url: None,
                connected: false,
            },
            ConnectProfileItem {
                id: "anthropic".into(),
                title: "Anthropic".into(),
                auth_mode: "api_key".into(),
                auth_url: None,
                connected: false,
            },
            ConnectProfileItem {
                id: "openai".into(),
                title: "OpenAI".into(),
                auth_mode: "api_key".into(),
                auth_url: None,
                connected: false,
            },
        ]);

        let Overlay::ConnectPicker { items, .. } = overlay else {
            panic!("expected connect picker");
        };
        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["anthropic", "openai", "xai"]
        );
    }

    #[test]
    fn model_picker_filters_moves_providers_and_focuses_current_model() {
        let mut overlay = Overlay::model_open_with(vec![
            ModelItem {
                provider: "native".into(),
                model: "openai/gpt-5".into(),
                profile_id: Some("openai".into()),
                source: forge_connect::CatalogSource::Live,
            },
            ModelItem {
                provider: "native".into(),
                model: "anthropic/claude-sonnet".into(),
                profile_id: Some("anthropic".into()),
                source: forge_connect::CatalogSource::Registry,
            },
            ModelItem {
                provider: "native".into(),
                model: "ollama/llama3".into(),
                profile_id: Some("ollama".into()),
                source: forge_connect::CatalogSource::Default,
            },
        ]);

        overlay.focus_model("ollama/llama3");
        let Overlay::Model {
            provider_selected,
            model_selected,
            current_model,
            providers,
            ..
        } = &overlay
        else {
            panic!("expected model overlay");
        };
        assert_eq!(providers[*provider_selected], "ollama");
        assert_eq!(*model_selected, 0);
        assert_eq!(current_model, "ollama/llama3");

        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Left),
            OverlayAction::None
        );
        let Overlay::Model {
            provider_selected,
            model_selected,
            providers,
            ..
        } = &overlay
        else {
            panic!("expected model overlay");
        };
        assert_eq!(providers[*provider_selected], "anthropic");
        assert_eq!(*model_selected, 0);

        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Char('g')),
            OverlayAction::None
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::RunCommand("/model g".into())
        );
    }

    fn shared_route_items() -> Vec<ModelItem> {
        vec![
            ModelItem {
                provider: "native".into(),
                model: "openai/gpt-5.6".into(),
                profile_id: Some("openai".into()),
                source: forge_connect::CatalogSource::Live,
            },
            ModelItem {
                provider: "native".into(),
                model: "openai/gpt-5.6".into(),
                profile_id: Some("openrouter".into()),
                source: forge_connect::CatalogSource::Live,
            },
        ]
    }

    #[test]
    fn enter_on_multi_route_model_opens_route_picker_instead_of_selecting() {
        let mut overlay = Overlay::model_open_with(shared_route_items());
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::None,
            "an ambiguous model must not be applied without disambiguation"
        );
        let Overlay::Model { route_picker, .. } = &overlay else {
            panic!("expected model overlay");
        };
        let rp = route_picker.as_ref().expect("route picker should open");
        assert_eq!(rp.model_id, "gpt-5.6");
        assert_eq!(rp.routes.len(), 2);
    }

    #[test]
    fn route_picker_enter_selects_the_highlighted_route() {
        let mut overlay = Overlay::model_open_with(shared_route_items());
        handle_overlay_key(&mut overlay, Key::Enter); // open route picker
        handle_overlay_key(&mut overlay, Key::Down); // move to the second route
        let action = handle_overlay_key(&mut overlay, Key::Enter);
        assert_eq!(
            action,
            OverlayAction::SelectModel {
                provider: "native".into(),
                model: "openai/gpt-5.6".into(),
                profile_id: Some("openrouter".into()),
            }
        );
    }

    #[test]
    fn route_picker_esc_returns_to_model_list_without_closing_overlay() {
        let mut overlay = Overlay::model_open_with(shared_route_items());
        handle_overlay_key(&mut overlay, Key::Enter);
        assert!(matches!(
            &overlay,
            Overlay::Model {
                route_picker: Some(_),
                ..
            }
        ));
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Esc),
            OverlayAction::None
        );
        assert!(matches!(
            &overlay,
            Overlay::Model {
                route_picker: None,
                ..
            }
        ));
    }

    #[test]
    fn single_route_model_still_selects_immediately_on_enter() {
        let mut overlay = Overlay::model_open_with(vec![ModelItem {
            provider: "native".into(),
            model: "openai/gpt-4.1-mini".into(),
            profile_id: Some("openai".into()),
            source: forge_connect::CatalogSource::Live,
        }]);
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::SelectModel {
                provider: "native".into(),
                model: "openai/gpt-4.1-mini".into(),
                profile_id: Some("openai".into()),
            }
        );
    }

    #[test]
    fn tab_toggles_route_reveal_for_the_highlighted_group() {
        let mut overlay = Overlay::model_open_with(shared_route_items());
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Tab),
            OverlayAction::None
        );
        let Overlay::Model { groups, .. } = &overlay else {
            panic!("expected model overlay");
        };
        assert!(groups[0].expanded, "Tab should reveal routes inline");
        let expanded_text = render_text(&overlay);
        assert!(expanded_text.contains("via openai"));
        assert!(expanded_text.contains("via openrouter"));

        handle_overlay_key(&mut overlay, Key::Tab);
        let Overlay::Model { groups, .. } = &overlay else {
            panic!("expected model overlay");
        };
        assert!(!groups[0].expanded, "a second Tab collapses it again");
    }

    #[test]
    fn multi_route_row_shows_route_count_badge() {
        let overlay = Overlay::model_open_with(shared_route_items());
        let text = render_text(&overlay);
        assert!(text.contains("2 routes"));
    }

    #[test]
    fn model_picker_footer_shows_current_effort_with_extra_high_label() {
        let mut overlay = Overlay::model_open_with(shared_route_items());
        overlay.set_current_effort(ReasoningEffort::XHigh);
        let text = render_text(&overlay);
        assert!(text.contains("Effort: Extra High"));
    }

    #[test]
    fn empty_pickers_and_lists_ignore_selection_and_enter() {
        let mut effort = Overlay::Effort {
            model: "unknown/model".into(),
            selected: 0,
            current: ReasoningEffort::Auto,
            default: ReasoningEffort::Auto,
            items: vec![],
        };
        assert_eq!(
            handle_overlay_key(&mut effort, Key::Down),
            OverlayAction::None
        );
        assert_eq!(
            handle_overlay_key(&mut effort, Key::Enter),
            OverlayAction::None
        );

        let mut connect = Overlay::connect_picker(vec![]);
        assert_eq!(
            handle_overlay_key(&mut connect, Key::Down),
            OverlayAction::None
        );
        assert_eq!(
            handle_overlay_key(&mut connect, Key::Enter),
            OverlayAction::None
        );

        let mut resume = Overlay::resume_picker(vec![]);
        assert_eq!(
            handle_overlay_key(&mut resume, Key::Up),
            OverlayAction::None
        );
        assert_eq!(
            handle_overlay_key(&mut resume, Key::Enter),
            OverlayAction::None
        );

        let mut files = Overlay::file_explorer("/", vec![], None);
        assert_eq!(
            handle_overlay_key(&mut files, Key::Down),
            OverlayAction::None
        );
        assert_eq!(
            handle_overlay_key(&mut files, Key::Enter),
            OverlayAction::None
        );
    }

    #[test]
    fn theme_picker_selects_choice() {
        let mut overlay = Overlay::theme_open(Theme::Dark);
        handle_overlay_key(&mut overlay, Key::Down);
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::SelectTheme(Theme::Light)
        );
    }

    #[test]
    fn picker_selection_wraps_in_both_directions() {
        let mut overlay = Overlay::resume_picker(vec![
            ResumeSessionItem {
                id: "one".into(),
                modified: "now".into(),
            },
            ResumeSessionItem {
                id: "two".into(),
                modified: "then".into(),
            },
        ]);

        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Up),
            OverlayAction::None
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::RunCommand("/resume two".into())
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Down),
            OverlayAction::None
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::RunCommand("/resume one".into())
        );
    }

    #[test]
    fn overlay_widget_renders_help_effort_status_and_turn_limit() {
        let help = render_text(&Overlay::Help);
        assert!(help.contains("Forge is an AI coding agent"));
        assert!(help.contains("⇧← / ⇧→"));

        let effort = render_text(&Overlay::effort_open("openai/gpt-5", ReasoningEffort::High));
        assert!(effort.contains("Reasoning effort"));
        assert!(effort.contains("Model: openai/gpt-5"));
        assert!(effort.contains("Esc close"));

        let status = render_text(&Overlay::StatusReport {
            title: "Status".into(),
            lines: vec!["one".into(), "two".into()],
        });
        assert!(status.contains("Status"));
        assert!(status.contains("one"));
        assert!(status.contains("two"));

        let turn_limit = render_text(&Overlay::turn_limit(64));
        assert!(turn_limit.contains("Turn limit reached"));
        assert!(turn_limit.contains("64 model steps"));
        assert!(turn_limit.contains("[n/Esc] Stop"));
    }

    #[test]
    fn overlay_widget_renders_hitl_collapsed_and_expanded() {
        let payload = HitlPayload {
            call_id: "call-1".into(),
            tool: "write".into(),
            args_redacted: json!({"path": "src/main.rs"}),
            reason: "Edit requires approval".into(),
        };
        let collapsed = render_text(&Overlay::hitl(payload.clone()));
        assert!(collapsed.contains("Approval required"));
        assert!(collapsed.contains("Mode: Direct"));
        assert!(collapsed.contains("Executable: write"));
        assert!(collapsed.contains("Working directory: workspace"));
        assert!(collapsed.contains("[Allow once]"));
        assert!(collapsed.contains("[Deny]"));
        assert!(collapsed.contains("Remember this exact Direct invocation in this workspace"));
        assert!(collapsed.contains("v view details"));

        let mut expanded_overlay = Overlay::hitl(payload);
        if let Overlay::Hitl { expanded, .. } = &mut expanded_overlay {
            *expanded = true;
        }
        let expanded = render_text(&expanded_overlay);
        assert!(expanded.contains("details"));
        assert!(expanded.contains("Tool: write"));
        assert!(expanded.contains("Secrets are not shown"));
    }

    #[test]
    fn overlay_widget_renders_model_empty_states() {
        let empty_model = render_text(&Overlay::model_open_with(vec![]));
        assert!(empty_model.contains("Models · all · 0/0"));
        assert!(empty_model.contains("No models are available"));

        let mut filtered_model = Overlay::model_open_with(vec![ModelItem {
            provider: "native".into(),
            model: "openai/gpt-5".into(),
            profile_id: Some("openai".into()),
            source: forge_connect::CatalogSource::Registry,
        }]);
        handle_overlay_key(&mut filtered_model, Key::Char('z'));
        let text = render_text(&filtered_model);
        assert!(text.contains("/model z"));
        assert!(text.contains("No models match"));
    }

    #[test]
    fn overlay_widget_renders_connect_and_resume_variants() {
        let mut api = Overlay::connect_api_key(
            "opencode_go",
            "OpenCode Go",
            Some("https://example.test/key".into()),
            Some("OPENCODE_API_KEY".into()),
        );
        handle_overlay_key(&mut api, Key::Paste("sk-secret".into()));
        let api_text = render_text(&api);
        assert!(api_text.contains("Connect with API key"));
        assert!(api_text.contains("OpenCode Go"));
        assert!(api_text.contains("9 chars"));
        assert!(api_text.contains("[e] Use OPENCODE_API_KEY"));
        assert!(!api_text.contains("sk-secret"));

        let oauth = render_text(&Overlay::connect_oauth(
            "xai",
            "xAI Grok",
            "Open the browser code",
        ));
        assert!(oauth.contains("Sign in"));
        assert!(oauth.contains("Open the browser code"));
        assert!(oauth.contains("Enter check now"));

        let picker = render_text(&Overlay::connect_picker(vec![
            ConnectProfileItem {
                id: "ollama".into(),
                title: "Ollama".into(),
                auth_mode: "api_key".into(),
                auth_url: None,
                connected: true,
            },
            ConnectProfileItem {
                id: "xai".into(),
                title: "xAI Grok".into(),
                auth_mode: "oauth".into(),
                auth_url: None,
                connected: false,
            },
        ]));
        assert!(picker.contains("Choose a provider"));
        assert!(picker.contains("Ollama"));
        assert!(picker.contains("connected"));
        assert!(picker.contains("Sign in"));

        let resume = render_text(&Overlay::resume_picker(vec![ResumeSessionItem {
            id: "session-123".into(),
            modified: "2026-07-29 05:00".into(),
        }]));
        assert!(resume.contains("Resume a session"));
        assert!(resume.contains("session-123"));
    }

    #[test]
    fn overlay_widget_renders_file_explorer_and_viewer() {
        let explorer = render_text(&Overlay::file_explorer(
            "/workspace",
            vec![
                FileExplorerItem {
                    name: "src".into(),
                    path: "/workspace/src".into(),
                    is_dir: true,
                },
                FileExplorerItem {
                    name: "README.md".into(),
                    path: "/workspace/README.md".into(),
                    is_dir: false,
                },
            ],
            Some("unable to read".into()),
        ));
        assert!(explorer.contains("File explorer"));
        assert!(explorer.contains("/workspace"));
        assert!(explorer.contains("src"));
        assert!(explorer.contains("unable to read"));

        let mut viewer = Overlay::file_viewer("/workspace/README.md", "line 1\nline 2\nline 3");
        handle_overlay_key(&mut viewer, Key::Down);
        let text = render_text(&viewer);
        assert!(text.contains("/workspace/README.md"));
        assert!(text.contains("readonly"));
        assert!(text.contains("line 2"));
        assert!(text.contains("←/Backspace back"));
    }

    #[test]
    fn hitl_command_and_risk_summaries_cover_special_tools() {
        assert_eq!(
            hitl_command("bash", &json!({"command": "cargo test"})),
            "$ cargo test"
        );
        assert_eq!(
            hitl_command("edit", &json!({"path": "src/lib.rs", "old": "a"})),
            "edit  src/lib.rs"
        );
        assert_eq!(
            hitl_risk_summary("git", &json!({"command": "push origin main"})),
            "This pushes changes to a remote repository."
        );
        assert_eq!(
            hitl_risk_summary("npm_install", &json!({})),
            "This installs or modifies system packages."
        );
        assert_eq!(
            hitl_risk_summary("sql", &json!({})),
            "This executes a database query."
        );
    }
}
