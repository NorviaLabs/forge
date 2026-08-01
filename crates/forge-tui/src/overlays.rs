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
    /// Unified Connect + Model + Effort picker (`/connect` and `/model` both
    /// open this, differing only in `focus`) — one state source so the two
    /// commands can never disagree about "current".
    ConnectModel {
        providers: Vec<ProviderVendorRow>,
        /// Index into `flatten_provider_rows(providers)`.
        provider_cursor: usize,
        /// Connect profile id scoping `groups`; `None` before any route has
        /// ever been picked (e.g. first-ever `/connect`).
        selected_route: Option<String>,
        /// Unscoped catalog across every reachable profile; re-filtered into
        /// `groups` whenever `selected_route` changes.
        all_items: Vec<ModelItem>,
        groups: Vec<ModelGroup>,
        model_input: String,
        model_selected: usize,
        effort_items: Vec<ReasoningEffort>,
        effort_selected: usize,
        active_profile_id: Option<String>,
        active_model: String,
        active_effort: ReasoningEffort,
        focus: ConnectModelColumn,
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

/// Which column of the unified Connect + Model picker has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectModelColumn {
    Providers,
    Models,
    Effort,
}

impl ConnectModelColumn {
    fn next(self) -> Self {
        match self {
            Self::Providers => Self::Models,
            Self::Models => Self::Effort,
            Self::Effort => Self::Providers,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Providers => Self::Effort,
            Self::Models => Self::Providers,
            Self::Effort => Self::Models,
        }
    }
}

/// One offering nested under a vendor row, e.g. "API key" / "ChatGPT sign-in".
#[derive(Debug, Clone)]
pub struct ProviderRouteRow {
    pub profile_id: String,
    pub label: String,
    pub connected: bool,
    pub is_current: bool,
}

/// One vendor row in the Providers column. Vendors with a single offering
/// carry it as `routes[0]` and render flat, with no chevron; vendors with
/// more than one offering render as an expandable header with `routes`
/// nested underneath — the same affordance the file tree uses.
#[derive(Debug, Clone)]
pub struct ProviderVendorRow {
    pub vendor_id: String,
    pub label: String,
    pub routes: Vec<ProviderRouteRow>,
    pub expanded: bool,
}

/// One visible row of the flattened Providers column: a vendor header, or
/// (when expanded) one of its nested routes. Mirrors `file_explorer`'s
/// `flatten`/`VisibleNode` pattern for the same chevron interaction.
struct ProviderFlatRow {
    vendor_idx: usize,
    /// `None` selects the vendor header itself; `Some(i)` selects `routes[i]`.
    route_idx: Option<usize>,
}

fn flatten_provider_rows(providers: &[ProviderVendorRow]) -> Vec<ProviderFlatRow> {
    let mut out = Vec::new();
    for (vendor_idx, vendor) in providers.iter().enumerate() {
        out.push(ProviderFlatRow {
            vendor_idx,
            route_idx: None,
        });
        if vendor.routes.len() > 1 && vendor.expanded {
            for route_idx in 0..vendor.routes.len() {
                out.push(ProviderFlatRow {
                    vendor_idx,
                    route_idx: Some(route_idx),
                });
            }
        }
    }
    out
}

/// The connect profile a flattened row represents, if any — `None` for a
/// multi-route vendor's header row, which only toggles expand/collapse.
fn flat_row_profile<'a>(
    providers: &'a [ProviderVendorRow],
    row: &ProviderFlatRow,
) -> Option<&'a ProviderRouteRow> {
    let vendor = providers.get(row.vendor_idx)?;
    match row.route_idx {
        Some(i) => vendor.routes.get(i),
        None if vendor.routes.len() == 1 => vendor.routes.first(),
        None => None,
    }
}

/// Build the Providers column from the registry, grouped by vendor. Only the
/// vendor owning `current_profile_id` starts expanded — the same "expand the
/// active path, collapse the rest" rule the file tree already uses.
pub fn build_provider_rows(
    registry: &forge_connect::ConnectRegistry,
    connected: &std::collections::HashSet<String>,
    current_profile_id: Option<&str>,
) -> Vec<ProviderVendorRow> {
    let mut vendor_order: Vec<(String, String)> = Vec::new();
    let mut routes_by_vendor: std::collections::BTreeMap<String, Vec<ProviderRouteRow>> =
        std::collections::BTreeMap::new();
    for p in registry.profiles() {
        if !vendor_order.iter().any(|(id, _)| id == &p.vendor_id) {
            vendor_order.push((p.vendor_id.clone(), p.vendor_label.clone()));
        }
        routes_by_vendor
            .entry(p.vendor_id.clone())
            .or_default()
            .push(ProviderRouteRow {
                profile_id: p.id.clone(),
                label: if p.route_label.is_empty() {
                    p.title.clone()
                } else {
                    p.route_label.clone()
                },
                connected: connected.contains(&p.id),
                is_current: current_profile_id == Some(p.id.as_str()),
            });
    }
    vendor_order.sort_by_key(|(_, label)| label.to_ascii_lowercase());
    let current_vendor_id = current_profile_id
        .and_then(|pid| registry.get(pid))
        .map(|p| p.vendor_id.clone());
    vendor_order
        .into_iter()
        .map(|(vendor_id, label)| {
            let routes = routes_by_vendor.remove(&vendor_id).unwrap_or_default();
            let expanded =
                routes.len() > 1 && current_vendor_id.as_deref() == Some(vendor_id.as_str());
            ProviderVendorRow {
                vendor_id,
                label,
                routes,
                expanded,
            }
        })
        .collect()
}

/// The active vendor/route labels for the picker's box title and Active
/// line — `None` route means the vendor has only one offering.
fn active_vendor_route_labels<'a>(
    providers: &'a [ProviderVendorRow],
    active_profile_id: Option<&str>,
) -> Option<(&'a str, Option<&'a str>)> {
    let pid = active_profile_id?;
    providers.iter().find_map(|vendor| {
        vendor
            .routes
            .iter()
            .find(|r| r.profile_id == pid)
            .map(|route| {
                let route_label = (vendor.routes.len() > 1).then_some(route.label.as_str());
                (vendor.label.as_str(), route_label)
            })
    })
}

/// First visible index so `selected` stays on-screen within `visible` rows.
fn window_start(selected: usize, total: usize, visible: usize) -> usize {
    if visible == 0 || total <= visible || selected < visible {
        0
    } else {
        (selected + 1).saturating_sub(visible)
    }
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

    /// Build items scoped to `route` (or every reachable item when `route`
    /// is `None`, e.g. before any provider has ever been picked).
    fn scoped_groups(items: &[ModelItem], route: Option<&str>) -> Vec<ModelGroup> {
        let filtered: Vec<ModelItem> = match route {
            Some(pid) => items
                .iter()
                .filter(|m| m.profile_id.as_deref() == Some(pid))
                .cloned()
                .collect(),
            None => items.to_vec(),
        };
        group_model_items(filtered)
    }

    fn index_of_model(groups: &[ModelGroup], model: &str) -> usize {
        if model.is_empty() {
            return 0;
        }
        groups
            .iter()
            .position(|g| g.routes.iter().any(|m| m.model == model))
            .unwrap_or(0)
    }

    fn provider_cursor_for(providers: &[ProviderVendorRow], profile_id: Option<&str>) -> usize {
        let Some(pid) = profile_id else { return 0 };
        flatten_provider_rows(providers)
            .iter()
            .position(|row| {
                flat_row_profile(providers, row).is_some_and(|route| route.profile_id == pid)
            })
            .unwrap_or(0)
    }

    /// Build the unified Connect + Model + Effort picker. `/connect` and
    /// `/model` both call this, differing only in `focus`.
    pub fn connect_model_open(
        providers: Vec<ProviderVendorRow>,
        items: Vec<ModelItem>,
        current_profile_id: Option<&str>,
        current_model: &str,
        current_effort: ReasoningEffort,
        focus: ConnectModelColumn,
    ) -> Self {
        let selected_route = current_profile_id.map(str::to_string);
        let groups = Self::scoped_groups(&items, selected_route.as_deref());
        let model_selected = Self::index_of_model(&groups, current_model);
        let effort_items = effort_options(current_model);
        let default_effort = ReasoningEffort::default_for_model(current_model);
        let effort_selected = effort_items
            .iter()
            .position(|e| *e == current_effort)
            .or_else(|| effort_items.iter().position(|e| *e == default_effort))
            .unwrap_or(0);
        let provider_cursor = Self::provider_cursor_for(&providers, current_profile_id);
        Self::ConnectModel {
            providers,
            provider_cursor,
            selected_route,
            all_items: items,
            groups,
            model_input: String::new(),
            model_selected,
            effort_items,
            effort_selected,
            active_profile_id: current_profile_id.map(str::to_string),
            active_model: current_model.to_string(),
            active_effort: current_effort,
            focus,
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
            Self::ConnectModel {
                providers,
                provider_cursor,
                groups,
                model_input,
                model_selected,
                effort_items,
                effort_selected,
                focus,
                ..
            } => match focus {
                ConnectModelColumn::Providers => {
                    let n = flatten_provider_rows(providers).len().max(1) as i32;
                    *provider_cursor = ((*provider_cursor as i32 + delta).rem_euclid(n)) as usize;
                }
                ConnectModelColumn::Models => {
                    let n = groups
                        .iter()
                        .filter(|g| group_matches_input(model_input, g))
                        .count()
                        .max(1) as i32;
                    *model_selected = ((*model_selected as i32 + delta).rem_euclid(n)) as usize;
                }
                ConnectModelColumn::Effort => {
                    if !effort_items.is_empty() {
                        let n = effort_items.len() as i32;
                        *effort_selected =
                            ((*effort_selected as i32 + delta).rem_euclid(n)) as usize;
                    }
                }
            },
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

fn model_matches_input(model_input: &str, item: &ModelItem) -> bool {
    let needle = model_input.trim().to_ascii_lowercase();
    needle.is_empty() || item.model.to_ascii_lowercase().contains(&needle)
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
            OverlayAction::None
        }
        Key::Right => {
            if let Overlay::Hitl { approval, .. } = overlay {
                approval.focus_next(1);
                return OverlayAction::None;
            }
            OverlayAction::None
        }
        Key::Enter => match overlay {
            Overlay::Help => OverlayAction::BeginOnboarding,
            Overlay::StatusReport { .. } => OverlayAction::Close,
            Overlay::TurnLimit { .. } => OverlayAction::ContinueTurns,
            Overlay::ConnectModel {
                providers,
                provider_cursor,
                selected_route,
                all_items,
                groups,
                model_input,
                model_selected,
                effort_items,
                effort_selected,
                active_profile_id,
                active_model,
                active_effort,
                focus,
            } => match focus {
                ConnectModelColumn::Providers => {
                    let rows = flatten_provider_rows(providers);
                    let Some(row) = rows.get(*provider_cursor) else {
                        return OverlayAction::None;
                    };
                    let vendor = &providers[row.vendor_idx];
                    if row.route_idx.is_none() && vendor.routes.len() > 1 {
                        // Multi-route vendor header: toggle expand/collapse in place.
                        let expanded = !vendor.expanded;
                        providers[row.vendor_idx].expanded = expanded;
                        return OverlayAction::None;
                    }
                    let Some(route) = flat_row_profile(providers, row) else {
                        return OverlayAction::None;
                    };
                    if !route.connected {
                        return OverlayAction::ConnectPickProfile {
                            profile_id: route.profile_id.clone(),
                        };
                    }
                    // Already connected: scope the Models column to this route
                    // in place, no app-level action needed.
                    let profile_id = route.profile_id.clone();
                    *selected_route = Some(profile_id.clone());
                    *groups = Overlay::scoped_groups(all_items, Some(profile_id.as_str()));
                    model_input.clear();
                    *model_selected = Overlay::index_of_model(groups, active_model);
                    *focus = ConnectModelColumn::Models;
                    OverlayAction::None
                }
                ConnectModelColumn::Models => {
                    let chosen = groups
                        .iter()
                        .filter(|g| group_matches_input(model_input, g))
                        .nth(*model_selected);
                    let typed = model_input.trim();
                    if !typed.is_empty()
                        && !chosen.is_some_and(|g| {
                            g.routes.iter().any(|m| m.model.eq_ignore_ascii_case(typed))
                        })
                    {
                        // No catalog match for the typed text — let the caller
                        // re-dispatch it as a free-text `/model <arg>` for
                        // advanced users naming an unlisted model.
                        return OverlayAction::RunCommand(format!("/model {typed}"));
                    }
                    let Some(g) = chosen else {
                        return OverlayAction::None;
                    };
                    // A route was already resolved via the Providers column, so
                    // there should be exactly one; if a catalog quirk still
                    // yields more than one, prefer the account-verified entry.
                    let Some(route) = g
                        .routes
                        .iter()
                        .find(|r| r.source == forge_connect::CatalogSource::Live)
                        .or_else(|| g.routes.first())
                    else {
                        return OverlayAction::None;
                    };
                    *active_model = route.model.clone();
                    if let Some(pid) = route.profile_id.clone().or_else(|| selected_route.clone()) {
                        *active_profile_id = Some(pid);
                    }
                    let opts = effort_options(&route.model);
                    let default_effort = ReasoningEffort::default_for_model(&route.model);
                    let use_effort = if opts.contains(active_effort) {
                        *active_effort
                    } else {
                        default_effort
                    };
                    *effort_selected = opts.iter().position(|e| *e == use_effort).unwrap_or(0);
                    *active_effort = use_effort;
                    *effort_items = opts;
                    *focus = ConnectModelColumn::Effort;
                    OverlayAction::SelectModel {
                        provider: route.provider.clone(),
                        model: route.model.clone(),
                        profile_id: route.profile_id.clone(),
                    }
                }
                ConnectModelColumn::Effort => effort_items
                    .get(*effort_selected)
                    .copied()
                    .map(OverlayAction::SelectEffort)
                    .unwrap_or(OverlayAction::None),
            },
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
                Overlay::ConnectModel {
                    focus: ConnectModelColumn::Models,
                    ..
                }
            ) =>
        {
            if let Overlay::ConnectModel {
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
                Overlay::ConnectModel {
                    focus: ConnectModelColumn::Models,
                    ..
                }
            ) =>
        {
            if let Overlay::ConnectModel {
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
                Overlay::ConnectModel {
                    focus: ConnectModelColumn::Models,
                    ..
                }
            ) =>
        {
            if let Overlay::ConnectModel {
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
        Key::Tab if matches!(overlay, Overlay::ConnectModel { .. }) => {
            if let Overlay::ConnectModel { focus, .. } = overlay {
                *focus = focus.next();
            }
            OverlayAction::None
        }
        Key::BackTab if matches!(overlay, Overlay::ConnectModel { .. }) => {
            if let Overlay::ConnectModel { focus, .. } = overlay {
                *focus = focus.prev();
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
        match self.overlay {
            Overlay::Hitl { .. } => {}
            // Dim the transcript in place instead of blanking it, so it stays
            // legible-but-muted behind the picker rather than disappearing.
            Overlay::ConnectModel { .. } => theme::dim_region(area, buf),
            _ => theme::fill(area, buf, theme::canvas()),
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
            Overlay::ConnectModel {
                providers,
                provider_cursor,
                groups,
                model_input,
                model_selected,
                effort_items,
                effort_selected,
                active_profile_id,
                active_model,
                active_effort,
                focus,
                ..
            } => {
                let r = centered_capped_rect(area, 100, 22);
                let active = active_vendor_route_labels(providers, active_profile_id.as_deref());
                let label_suffix = match active {
                    Some((vendor, Some(route))) => format!(" · {vendor} · {route}"),
                    Some((vendor, None)) => format!(" · {vendor}"),
                    None => String::new(),
                };
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::border())
                    .style(theme::panel())
                    .title(Span::styled(
                        format!(" Connect & Model{label_suffix} "),
                        theme::brand(),
                    ));
                let inner = block.inner(r);
                block.render(r, buf);
                let regions = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(1),
                        Constraint::Min(3),
                        Constraint::Length(1),
                        Constraint::Length(1),
                    ])
                    .split(inner);
                let col_constraints = [
                    Constraint::Percentage(30),
                    Constraint::Length(2),
                    Constraint::Percentage(38),
                    Constraint::Length(2),
                    Constraint::Percentage(30),
                ];
                let header_cols = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints(col_constraints)
                    .split(regions[0]);
                let body_cols = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints(col_constraints)
                    .split(regions[1]);
                let (providers_header, models_header, effort_header) =
                    (header_cols[0], header_cols[2], header_cols[4]);
                let (providers_area, models_area, effort_area) =
                    (body_cols[0], body_cols[2], body_cols[4]);

                let head_style = |col: ConnectModelColumn| {
                    if *focus == col {
                        theme::brand()
                    } else {
                        theme::muted()
                    }
                };
                Paragraph::new("PROVIDERS")
                    .style(head_style(ConnectModelColumn::Providers))
                    .render(providers_header, buf);
                Paragraph::new("MODELS")
                    .style(head_style(ConnectModelColumn::Models))
                    .render(models_header, buf);
                Paragraph::new("EFFORT")
                    .style(head_style(ConnectModelColumn::Effort))
                    .render(effort_header, buf);

                // Providers column.
                let flat = flatten_provider_rows(providers);
                let visible = providers_area.height.max(1) as usize;
                let start = window_start(*provider_cursor, flat.len(), visible);
                let end = (start + visible).min(flat.len());
                let provider_items: Vec<ListItem> = flat[start..end]
                    .iter()
                    .enumerate()
                    .map(|(i, row)| {
                        let idx = start + i;
                        let vendor = &providers[row.vendor_idx];
                        let selected = idx == *provider_cursor;
                        let style = if selected {
                            theme::focused_selection_style()
                        } else {
                            theme::text()
                        };
                        let indent = if row.route_idx.is_some() { "  " } else { "" };
                        let glyph = match row.route_idx {
                            None if vendor.routes.len() > 1 => {
                                if vendor.expanded {
                                    "▾"
                                } else {
                                    "▸"
                                }
                            }
                            _ => " ",
                        };
                        let label = match row.route_idx {
                            Some(i) => vendor.routes[i].label.as_str(),
                            None => vendor.label.as_str(),
                        };
                        let tag = match flat_row_profile(providers, row) {
                            Some(route) if route.is_current => "current",
                            Some(route) if route.connected => "connected",
                            _ => "",
                        };
                        let marker = if selected { "▶ " } else { "  " };
                        let mut text = format!("{marker}{indent}{glyph} {label}");
                        if tag.is_empty() {
                            ListItem::new(Span::styled(text, style))
                        } else {
                            let target = (providers_area.width as usize)
                                .saturating_sub(tag.chars().count() + 1);
                            while text.chars().count() < target {
                                text.push(' ');
                            }
                            ListItem::new(Line::from(vec![
                                Span::styled(text, style),
                                Span::styled(tag, theme::tag_style(selected)),
                            ]))
                        }
                    })
                    .collect();
                List::new(provider_items).render(providers_area, buf);

                // Models column: a type-ahead filter line, then the scoped catalog.
                let models_regions = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(1), Constraint::Min(1)])
                    .split(models_area);
                let filter_text = if model_input.is_empty() {
                    "type to filter…".to_string()
                } else {
                    model_input.clone()
                };
                Paragraph::new(filter_text)
                    .style(if model_input.is_empty() {
                        theme::dim()
                    } else {
                        theme::text()
                    })
                    .render(models_regions[0], buf);
                let models_list_area = models_regions[1];
                let filtered: Vec<&ModelGroup> = groups
                    .iter()
                    .filter(|g| group_matches_input(model_input, g))
                    .collect();
                let visible = models_list_area.height.max(1) as usize;
                let start = window_start(*model_selected, filtered.len(), visible);
                let end = (start + visible).min(filtered.len());
                let model_items: Vec<ListItem> = if filtered.is_empty() {
                    vec![ListItem::new(Span::styled(
                        if active_profile_id.is_none() {
                            "Pick a provider first."
                        } else {
                            "No models match this filter."
                        },
                        theme::muted(),
                    ))]
                } else {
                    filtered[start..end]
                        .iter()
                        .enumerate()
                        .map(|(i, g)| {
                            let idx = start + i;
                            let selected = idx == *model_selected;
                            let style = if selected {
                                theme::focused_selection_style()
                            } else {
                                theme::text()
                            };
                            let is_current = g.routes.iter().any(|m| m.model == *active_model);
                            let tag = if is_current {
                                "current"
                            } else {
                                match g.routes.first().map(|m| m.source) {
                                    Some(forge_connect::CatalogSource::Registry) => "known",
                                    _ => "cloud",
                                }
                            };
                            let marker = if selected { "▶ " } else { "  " };
                            let mut row = format!("{marker}{}", g.model_id);
                            let target = (models_list_area.width as usize)
                                .saturating_sub(tag.chars().count() + 1);
                            while row.chars().count() < target {
                                row.push(' ');
                            }
                            ListItem::new(Line::from(vec![
                                Span::styled(row, style),
                                Span::styled(tag, theme::tag_style(selected)),
                            ]))
                        })
                        .collect()
                };
                List::new(model_items).render(models_list_area, buf);

                // Effort column.
                let default_effort = ReasoningEffort::default_for_model(active_model);
                let effort_list_items: Vec<ListItem> = effort_items
                    .iter()
                    .enumerate()
                    .map(|(idx, effort)| {
                        let selected = idx == *effort_selected;
                        let style = if selected {
                            theme::focused_selection_style()
                        } else {
                            theme::text()
                        };
                        let marker = if selected { "▶ " } else { "  " };
                        let is_current = *effort == *active_effort;
                        let default_label = if *effort == default_effort {
                            " (default)"
                        } else {
                            ""
                        };
                        let base = format!("{marker}{}{default_label}", effort.label());
                        if is_current {
                            ListItem::new(Line::from(vec![
                                Span::styled(base, style),
                                Span::styled(" current", theme::tag_style(selected)),
                            ]))
                        } else {
                            ListItem::new(Span::styled(base, style))
                        }
                    })
                    .collect();
                List::new(effort_list_items).render(effort_area, buf);

                let active_line = match active {
                    Some((vendor, route)) => format!(
                        "Active: {}",
                        crate::widgets::status::format_provider_model_effort(
                            vendor,
                            route,
                            active_model,
                            active_effort.label(),
                        )
                    ),
                    None => "Active: not connected".to_string(),
                };
                Paragraph::new(active_line)
                    .style(theme::text())
                    .render(regions[2], buf);
                Paragraph::new("↑↓ navigate · Tab switch pane · Enter select · Esc close")
                    .style(theme::dim())
                    .render(regions[3], buf);
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
                        let is_current = *choice == *current;
                        let selected_row = index == *selected;
                        let style = if selected_row {
                            theme::focused_selection_style()
                        } else {
                            theme::text()
                        };
                        let base = format!("{marker}{} ({})", choice.title(), choice.label());
                        if is_current {
                            ListItem::new(Line::from(vec![
                                Span::styled(base, style),
                                Span::styled(" · current", theme::tag_style(selected_row)),
                            ]))
                        } else {
                            ListItem::new(Span::styled(base, style))
                        }
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

    /// Build a `ConnectModel` overlay the way `/model` would, unscoped to any
    /// route (mirrors the old `model_open_with`) — used by tests that only
    /// care about the Models column.
    fn model_overlay(items: Vec<ModelItem>, focus: ConnectModelColumn) -> Overlay {
        Overlay::connect_model_open(vec![], items, None, "", ReasoningEffort::default(), focus)
    }

    fn sample_default_models() -> Vec<ModelItem> {
        let mut items = Vec::new();
        for p in forge_connect::builtin_registry().profiles() {
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

    #[test]
    fn model_select() {
        let mut o = model_overlay(sample_default_models(), ConnectModelColumn::Models);
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
        let mut overlay = model_overlay(sample_default_models(), ConnectModelColumn::Models);
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
        let mut overlay = model_overlay(sample_default_models(), ConnectModelColumn::Models);
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
    fn build_provider_rows_sorts_vendors_and_groups_multi_offering_ones() {
        let registry = forge_connect::builtin_registry();
        let connected = std::collections::HashSet::new();
        let rows = build_provider_rows(&registry, &connected, None);

        let labels: Vec<String> = rows.iter().map(|r| r.label.to_ascii_lowercase()).collect();
        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted, "vendor rows sort alphabetically by label");

        let openai = rows
            .iter()
            .find(|r| r.vendor_id == "openai")
            .expect("openai vendor row");
        assert_eq!(openai.routes.len(), 2, "API key + ChatGPT sign-in");
        let opencode = rows
            .iter()
            .find(|r| r.vendor_id == "opencode")
            .expect("opencode vendor row");
        assert_eq!(opencode.routes.len(), 2, "Go + Zen");
        let anthropic = rows
            .iter()
            .find(|r| r.vendor_id == "anthropic")
            .expect("anthropic vendor row");
        assert_eq!(
            anthropic.routes.len(),
            1,
            "single-offering vendors don't nest"
        );
    }

    #[test]
    fn providers_column_enter_toggles_expand_for_multi_route_vendor() {
        let registry = forge_connect::builtin_registry();
        let connected = std::collections::HashSet::new();
        let providers = build_provider_rows(&registry, &connected, None);
        let openai_idx = providers
            .iter()
            .position(|p| p.vendor_id == "openai")
            .unwrap();
        assert!(!providers[openai_idx].expanded);

        let mut overlay = Overlay::connect_model_open(
            providers,
            vec![],
            None,
            "",
            ReasoningEffort::default(),
            ConnectModelColumn::Providers,
        );
        if let Overlay::ConnectModel {
            provider_cursor, ..
        } = &mut overlay
        {
            *provider_cursor = openai_idx;
        }
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::None
        );
        let Overlay::ConnectModel { providers, .. } = &overlay else {
            panic!("expected connect model overlay");
        };
        assert!(
            providers[openai_idx].expanded,
            "Enter on a multi-route vendor header toggles expand"
        );
    }

    #[test]
    fn providers_column_enter_on_unconnected_route_requests_connect() {
        let registry = forge_connect::builtin_registry();
        let connected = std::collections::HashSet::new();
        let providers = build_provider_rows(&registry, &connected, None);
        let anthropic_idx = providers
            .iter()
            .position(|p| p.vendor_id == "anthropic")
            .unwrap();
        let mut overlay = Overlay::connect_model_open(
            providers,
            vec![],
            None,
            "",
            ReasoningEffort::default(),
            ConnectModelColumn::Providers,
        );
        if let Overlay::ConnectModel {
            provider_cursor, ..
        } = &mut overlay
        {
            *provider_cursor = anthropic_idx;
        }
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::ConnectPickProfile {
                profile_id: "anthropic".into()
            }
        );
    }

    #[test]
    fn providers_column_enter_on_connected_route_scopes_models_and_focuses_models() {
        let mut registry = forge_connect::ConnectRegistry::new();
        registry.register(forge_connect::anthropic_profile());
        registry.register(forge_connect::ollama_profile());
        let connected: std::collections::HashSet<String> =
            ["anthropic".to_string(), "ollama".to_string()]
                .into_iter()
                .collect();
        let providers = build_provider_rows(&registry, &connected, Some("ollama"));
        let items = vec![
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
        ];
        let mut overlay = Overlay::connect_model_open(
            providers,
            items,
            Some("ollama"),
            "ollama/llama3",
            ReasoningEffort::default(),
            ConnectModelColumn::Providers,
        );
        // Cursor starts on the active profile's vendor (Ollama); move up to Anthropic.
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Up),
            OverlayAction::None
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::None,
            "picking an already-connected route mutates the overlay in place"
        );
        let Overlay::ConnectModel {
            selected_route,
            groups,
            focus,
            ..
        } = &overlay
        else {
            panic!("expected connect model overlay");
        };
        assert_eq!(selected_route.as_deref(), Some("anthropic"));
        assert_eq!(*focus, ConnectModelColumn::Models);
        assert!(groups.iter().all(|g| g
            .routes
            .iter()
            .all(|m| m.profile_id.as_deref() == Some("anthropic"))));

        // Typed text that matches nothing in the scoped catalog still falls
        // back to a free-text `/model <arg>` re-dispatch.
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Char('g')),
            OverlayAction::None
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::RunCommand("/model g".into())
        );
    }

    #[test]
    fn tab_and_backtab_cycle_column_focus() {
        let mut overlay = Overlay::connect_model_open(
            vec![],
            vec![],
            None,
            "",
            ReasoningEffort::default(),
            ConnectModelColumn::Providers,
        );
        handle_overlay_key(&mut overlay, Key::Tab);
        assert!(matches!(
            &overlay,
            Overlay::ConnectModel {
                focus: ConnectModelColumn::Models,
                ..
            }
        ));
        handle_overlay_key(&mut overlay, Key::Tab);
        assert!(matches!(
            &overlay,
            Overlay::ConnectModel {
                focus: ConnectModelColumn::Effort,
                ..
            }
        ));
        handle_overlay_key(&mut overlay, Key::BackTab);
        assert!(matches!(
            &overlay,
            Overlay::ConnectModel {
                focus: ConnectModelColumn::Models,
                ..
            }
        ));
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
    fn models_column_enter_prefers_live_source_when_a_group_still_has_multiple_routes() {
        // A route is normally resolved via the Providers column before the
        // Models column is ever scoped, so a group should have exactly one
        // route in practice — but if a catalog quirk still yields two (e.g.
        // the same bare model from two sources), Enter must pick one
        // deterministically instead of reviving a disambiguation submode.
        let mut items = shared_route_items();
        items[1].source = forge_connect::CatalogSource::Cached;
        let mut overlay = model_overlay(items, ConnectModelColumn::Models);
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::SelectModel {
                provider: "native".into(),
                model: "openai/gpt-5.6".into(),
                profile_id: Some("openai".into()),
            }
        );
    }

    #[test]
    fn single_route_model_still_selects_immediately_on_enter() {
        let mut overlay = model_overlay(
            vec![ModelItem {
                provider: "native".into(),
                model: "openai/gpt-4.1-mini".into(),
                profile_id: Some("openai".into()),
                source: forge_connect::CatalogSource::Live,
            }],
            ConnectModelColumn::Models,
        );
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
    fn effort_column_shows_current_effort_with_extra_high_label() {
        let overlay = Overlay::connect_model_open(
            vec![],
            shared_route_items(),
            None,
            "openai/gpt-5.6",
            ReasoningEffort::XHigh,
            ConnectModelColumn::Effort,
        );
        let text = render_text(&overlay);
        assert!(text.contains("Extra High"));
        assert!(text.contains("current"));
    }

    #[test]
    fn empty_pickers_and_lists_ignore_selection_and_enter() {
        for focus in [ConnectModelColumn::Providers, ConnectModelColumn::Models] {
            let mut overlay = Overlay::connect_model_open(
                vec![],
                vec![],
                None,
                "",
                ReasoningEffort::default(),
                focus,
            );
            assert_eq!(
                handle_overlay_key(&mut overlay, Key::Down),
                OverlayAction::None
            );
            assert_eq!(
                handle_overlay_key(&mut overlay, Key::Enter),
                OverlayAction::None
            );
        }

        // `effort_items` is derived from the model string, so it's never
        // actually empty via the normal constructor — clear it directly to
        // exercise the defensive empty-list path.
        let mut effort_overlay = Overlay::connect_model_open(
            vec![],
            vec![],
            None,
            "",
            ReasoningEffort::default(),
            ConnectModelColumn::Effort,
        );
        if let Overlay::ConnectModel {
            effort_items,
            effort_selected,
            ..
        } = &mut effort_overlay
        {
            effort_items.clear();
            *effort_selected = 0;
        }
        assert_eq!(
            handle_overlay_key(&mut effort_overlay, Key::Down),
            OverlayAction::None
        );
        assert_eq!(
            handle_overlay_key(&mut effort_overlay, Key::Enter),
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
    fn overlay_widget_renders_help_status_and_turn_limit() {
        let help = render_text(&Overlay::Help);
        assert!(help.contains("Forge is an AI coding agent"));
        assert!(help.contains("⇧← / ⇧→"));

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
        let empty_model = render_text(&model_overlay(vec![], ConnectModelColumn::Models));
        assert!(empty_model.contains("Pick a provider first."));

        let mut filtered_model = Overlay::connect_model_open(
            vec![],
            vec![ModelItem {
                provider: "native".into(),
                model: "openai/gpt-5".into(),
                profile_id: Some("openai".into()),
                source: forge_connect::CatalogSource::Registry,
            }],
            Some("openai"),
            "",
            ReasoningEffort::default(),
            ConnectModelColumn::Models,
        );
        handle_overlay_key(&mut filtered_model, Key::Char('z'));
        let text = render_text(&filtered_model);
        assert!(text.contains("No models match this filter."));
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

        let mut registry = forge_connect::ConnectRegistry::new();
        registry.register(forge_connect::ollama_profile());
        registry.register(forge_connect::xai_grok_profile());
        let connected: std::collections::HashSet<String> =
            ["ollama".to_string()].into_iter().collect();
        let providers = build_provider_rows(&registry, &connected, Some("ollama"));
        let picker = render_text(&Overlay::connect_model_open(
            providers,
            vec![],
            Some("ollama"),
            "ollama/llama3",
            ReasoningEffort::default(),
            ConnectModelColumn::Providers,
        ));
        assert!(picker.contains("Connect & Model"));
        assert!(picker.contains("Ollama"));
        assert!(picker.contains("current"));
        assert!(picker.contains("xAI Grok"));

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
