//! Overlays: HITL, slash palette, model picker (TUI-04).

use crate::{effort::ReasoningEffort, theme, theme_registry};
use forge_types::HitlPayload;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, List, ListItem, Padding, Paragraph, Row, Table, Widget,
};
use std::path::Path;

#[derive(Debug, Clone)]
pub enum Overlay {
    Help,
    StatusReport {
        title: String,
        lines: Vec<String>,
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
        /// A background catalog refresh is in flight for this overlay (see
        /// `TuiApp::start_catalog_refresh`/`refresh_open_picker_items`).
        /// Distinguishes "still loading" from "genuinely no matches" in the
        /// Models column's empty state.
        catalog_loading: bool,
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
        current: String,
        items: Vec<(String, String)>,
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
}

#[derive(Debug, Clone)]
pub struct FileExplorerItem {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

/// Which single view of the unified Connect + Model picker is showing.
/// Each entry point (a footer segment, `/connect`, `/model`, …) opens the
/// picker locked to exactly one of these — there is no cycling between them
/// within one open picker (see `docs/provider-model-effort-modal-restructure.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectModelColumn {
    Providers,
    Models,
    Effort,
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

/// One selectable row in the Models column: a single-route group's one row,
/// or one specific route of a multi-route group. Mirrors `ProviderFlatRow`'s
/// flatten-for-navigation pattern.
struct ModelFlatRow {
    group_idx: usize,
    /// `None` selects the group's only route (single-route groups always
    /// render as one row); `Some(i)` selects `routes[i]` of a multi-route
    /// group, each rendered as its own row so the route choice is explicit
    /// rather than silently auto-picked.
    route_idx: Option<usize>,
}

fn flatten_model_rows(groups: &[&ModelGroup]) -> Vec<ModelFlatRow> {
    let mut out = Vec::new();
    for (group_idx, g) in groups.iter().enumerate() {
        if g.routes.len() > 1 {
            for route_idx in 0..g.routes.len() {
                out.push(ModelFlatRow {
                    group_idx,
                    route_idx: Some(route_idx),
                });
            }
        } else {
            out.push(ModelFlatRow {
                group_idx,
                route_idx: None,
            });
        }
    }
    out
}

/// The specific route a flattened Models-column row represents.
fn flat_row_item<'a>(groups: &[&'a ModelGroup], row: &ModelFlatRow) -> Option<&'a ModelItem> {
    let g = *groups.get(row.group_idx)?;
    match row.route_idx {
        Some(i) => g.routes.get(i),
        None => g.routes.first(),
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

/// Like [`window_start`], but pins `selected` to the *top* visible row
/// instead of the bottom — used for the model list so the active model
/// lands on the first row as soon as the picker opens, rather than
/// scrolled to the bottom of the window the way `window_start` would place
/// it. Still clamps so the window never scrolls past the end of the list.
fn window_start_pin_top(selected: usize, total: usize, visible: usize) -> usize {
    if visible == 0 || total <= visible {
        0
    } else {
        selected.min(total - visible)
    }
}

#[derive(Debug, Clone)]
pub struct ResumeSessionItem {
    pub id: String,
    pub modified: String,
    /// First user message of the session, truncated — `None` when the
    /// journal couldn't be read/replayed cheaply. Falls back to showing
    /// just `id`/`modified` in that case.
    pub title: Option<String>,
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
    /// Pre-resolved "Vendor" or "Vendor · Route" display string for
    /// `profile_id`, e.g. "OpenAI" or "OpenAI · ChatGPT sign-in". Empty when
    /// unresolvable. Computed once when items are built (see
    /// `TuiApp::model_picker_items`, which has registry access) rather than
    /// per keystroke, and matched against by the model search alongside the
    /// bare model id.
    pub route_label: String,
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

/// Within each group, move the route belonging to the active profile to the
/// front.
///
/// The picker resets the selection to row 0 on every keystroke, so typing a
/// model name that several providers offer selects whichever route happened to
/// sort first — pressing Enter then silently moves the session to a different
/// provider and account than the one it was already on. Ordering the active
/// route first makes row 0 the "stay where I am" choice, so the highlight and
/// the Enter action agree and the fast path is no longer a trap. Every route
/// stays present and individually selectable.
fn promote_active_route(groups: &mut [ModelGroup], active_profile_id: Option<&str>) {
    let Some(active) = active_profile_id else {
        return;
    };
    for group in groups {
        if group.routes.len() < 2 {
            continue;
        }
        if let Some(pos) = group
            .routes
            .iter()
            .position(|route| route.profile_id.as_deref() == Some(active))
        {
            group.routes[..=pos].rotate_right(1);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalExecutionMode {
    Direct,
    Shell,
}

#[derive(Debug, Clone)]
pub struct ApprovalOverlayState {
    pub mode: ApprovalExecutionMode,
    pub executable_or_shell: String,
    pub arguments: Vec<String>,
    pub shell_command: Option<String>,
    pub working_directory: String,
    pub environment_delta: String,
    pub pattern_allow_eligible: bool,
}

impl ApprovalOverlayState {
    /// Reduce a payload to the strings the transcript shows for it.
    ///
    /// Lives here rather than in `conversation` because it needs the per-tool
    /// execution modes, which are approval-overlay knowledge. The transcript
    /// takes the result.
    pub fn request_view(
        payload: &HitlPayload,
        working_directory: impl Into<String>,
    ) -> crate::conversation::ApprovalRequestView {
        let approval = Self::for_payload(payload, working_directory);
        let command = match approval.mode {
            ApprovalExecutionMode::Shell => approval.shell_command.unwrap_or_default(),
            ApprovalExecutionMode::Direct => std::iter::once(approval.executable_or_shell.as_str())
                .chain(approval.arguments.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" "),
        };
        crate::conversation::ApprovalRequestView {
            tool: payload.tool.clone(),
            command,
            cwd: approval.working_directory,
            env_delta: approval.environment_delta,
        }
    }

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
                    .or_else(|| payload.args_redacted.get("cmd"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_owned()
            })
            .filter(|value| !value.is_empty());
        let environment_delta = approval_environment_delta(&payload.args_redacted);
        let redacted = contains_redacted_value(&payload.args_redacted);
        let pattern_allow_eligible = !redacted && environment_delta != "[REDACTED]";

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
            pattern_allow_eligible,
        }
    }
}

pub fn default_palette_items() -> Vec<PaletteItem> {
    // Keep in sync with `commands::parse_slash`.
    vec![
        PaletteItem {
            cmd: "/help".into(),
            desc: "Show help and keyboard shortcuts".into(),
        },
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
            cmd: "/status".into(),
            desc: "Show session status and diagnostics".into(),
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
            cmd: "/terminal".into(),
            desc: "Open the terminal panel (Ctrl+`)".into(),
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
            // Filled in by `TuiApp::model_picker_items`, the caller with
            // registry access to resolve `profile_id` into a vendor/route
            // label — this function stays registry-agnostic.
            route_label: String::new(),
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

    /// Replace a `ConnectModel` overlay's catalog items in place (e.g. once a
    /// background catalog refresh lands), re-scoping `groups` to whatever
    /// route was already selected. No-op for any other overlay variant.
    pub(crate) fn refresh_model_items(&mut self, items: Vec<ModelItem>) {
        if let Self::ConnectModel {
            all_items,
            groups,
            selected_route,
            active_profile_id,
            ..
        } = self
        {
            *groups = Self::scoped_groups(
                &items,
                selected_route.as_deref(),
                active_profile_id.as_deref(),
            );
            *all_items = items;
        }
    }

    /// Mark whether a background catalog refresh is in flight for this
    /// overlay, so the Models column can distinguish "still loading" from
    /// "genuinely no matches" in its empty state. No-op for any other
    /// overlay variant.
    pub(crate) fn set_catalog_loading(&mut self, loading: bool) {
        if let Self::ConnectModel {
            catalog_loading, ..
        } = self
        {
            *catalog_loading = loading;
        }
    }

    /// Build items scoped to `route` (or every reachable item when `route`
    /// is `None`, e.g. before any provider has ever been picked).
    ///
    /// `active_profile_id` only orders routes within a group — it never filters.
    fn scoped_groups(
        items: &[ModelItem],
        route: Option<&str>,
        active_profile_id: Option<&str>,
    ) -> Vec<ModelGroup> {
        let filtered: Vec<ModelItem> = match route {
            Some(pid) => items
                .iter()
                .filter(|m| m.profile_id.as_deref() == Some(pid))
                .cloned()
                .collect(),
            None => items.to_vec(),
        };
        let mut groups = group_model_items(filtered);
        promote_active_route(&mut groups, active_profile_id);
        groups
    }

    /// Index into the *flattened* rows `flatten_model_rows(groups)` would
    /// produce, not into `groups` itself — a multi-route group expands to
    /// one row per route, so counting groups instead of flattened rows
    /// drifts the initial selection off the active model as soon as any
    /// earlier group has more than one route.
    fn index_of_model(groups: &[ModelGroup], model: &str) -> usize {
        if model.is_empty() {
            return 0;
        }
        let mut idx = 0;
        for g in groups {
            if g.routes.len() > 1 {
                for route in &g.routes {
                    if route.model == model {
                        return idx;
                    }
                    idx += 1;
                }
            } else if let Some(route) = g.routes.first() {
                if route.model == model {
                    return idx;
                }
                idx += 1;
            }
        }
        0
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
    /// `/model` both call this, differing only in `focus`. Scoped to
    /// `current_profile_id`'s models by default (a deliberate, guided
    /// browse) — see `connect_model_open_compact` for the persistent footer
    /// control's cross-route search default.
    pub fn connect_model_open(
        providers: Vec<ProviderVendorRow>,
        items: Vec<ModelItem>,
        current_profile_id: Option<&str>,
        current_model: &str,
        current_effort: ReasoningEffort,
        focus: ConnectModelColumn,
    ) -> Self {
        Self::connect_model_open_impl(
            providers,
            items,
            current_profile_id,
            current_profile_id,
            current_model,
            current_effort,
            focus,
        )
    }

    /// The same picker with an explicit route scope, for guided handoff.
    pub fn connect_model_open_scoped(
        providers: Vec<ProviderVendorRow>,
        items: Vec<ModelItem>,
        current_profile_id: Option<&str>,
        route_scope: Option<&str>,
        current_model: &str,
        current_effort: ReasoningEffort,
        focus: ConnectModelColumn,
    ) -> Self {
        Self::connect_model_open_impl(
            providers,
            items,
            current_profile_id,
            route_scope,
            current_model,
            current_effort,
            focus,
        )
    }

    /// The persistent `[vendor] [model] [effort]` footer control's picker —
    /// same rendering as `connect_model_open`, just seeded unscoped
    /// (`route_scope: None`) so it searches every connected route's models
    /// immediately rather than narrowing to the active one first: "the model
    /// control opens a searchable list built from every currently connected
    /// profile/route, not just the active route."
    pub fn connect_model_open_compact(
        providers: Vec<ProviderVendorRow>,
        items: Vec<ModelItem>,
        current_profile_id: Option<&str>,
        current_model: &str,
        current_effort: ReasoningEffort,
        focus: ConnectModelColumn,
    ) -> Self {
        Self::connect_model_open_scoped(
            providers,
            items,
            current_profile_id,
            None,
            current_model,
            current_effort,
            focus,
        )
    }

    /// `current_profile_id` marks which row is "current" (`active_profile_id`,
    /// `provider_cursor`, cursor restoration via `index_of_model`) — always
    /// the real active profile. `route_scope` seeds `selected_route`/`groups`
    /// independently: `Some(id)` narrows the initial model list to that one
    /// route (the guided-browse default), `None` leaves it spanning every
    /// connected route (`connect_model_open_compact`'s search default).
    /// These used to be the same parameter, which is what made the compact
    /// control wrongly scope to one route whenever a profile was already
    /// active.
    fn connect_model_open_impl(
        providers: Vec<ProviderVendorRow>,
        items: Vec<ModelItem>,
        current_profile_id: Option<&str>,
        route_scope: Option<&str>,
        current_model: &str,
        current_effort: ReasoningEffort,
        focus: ConnectModelColumn,
    ) -> Self {
        let selected_route = route_scope.map(str::to_string);
        let groups = Self::scoped_groups(&items, selected_route.as_deref(), current_profile_id);
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
            catalog_loading: false,
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

    pub fn theme_open(current: &str) -> Self {
        let items = theme_registry::picker_entries(&theme::registry());
        let selected = items.iter().position(|(id, _)| id == current).unwrap_or(0);
        Self::Theme {
            selected,
            current: current.to_string(),
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
                    let filtered: Vec<&ModelGroup> = groups
                        .iter()
                        .filter(|g| group_matches_input(model_input, g))
                        .collect();
                    let n = flatten_model_rows(&filtered).len().max(1) as i32;
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
    needle.is_empty()
        || item.model.to_ascii_lowercase().contains(&needle)
        || item.route_label.to_ascii_lowercase().contains(&needle)
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
    /// The typed value is not present in an account-backed catalog.
    ModelNotInCatalog(String),
    SelectEffort(ReasoningEffort),
    /// Live-preview a theme while the picker stays open (no persist).
    PreviewTheme(String),
    /// Confirm the highlighted theme (persist + close).
    SelectTheme(String),
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
    /// Providers view: picked a route that's already connected but isn't
    /// the currently active one — switch to it and land in usable steady
    /// state (default model + safe effort), without a forced next step.
    SwitchToRoute {
        profile_id: String,
    },
    FilePick {
        path: String,
        is_dir: bool,
    },
}

/// Key handling for overlays. Each `Overlay` variant dispatches to its own
/// key handler; the returned `OverlayAction` is applied by the app.
fn theme_preview_action(overlay: &Overlay) -> OverlayAction {
    match overlay {
        Overlay::Theme {
            selected, items, ..
        } => items
            .get(*selected)
            .map(|(id, _)| OverlayAction::PreviewTheme(id.clone()))
            .unwrap_or(OverlayAction::None),
        _ => OverlayAction::None,
    }
}

pub fn handle_overlay_key(overlay: &mut Overlay, key: Key) -> OverlayAction {
    match key {
        Key::Esc if matches!(overlay, Overlay::TurnLimit { .. }) => OverlayAction::StopTurns,
        Key::Esc => OverlayAction::Close,
        Key::Up => {
            overlay.move_sel(-1);
            theme_preview_action(overlay)
        }
        Key::Down => {
            overlay.move_sel(1);
            theme_preview_action(overlay)
        }
        Key::Left => {
            if let Some(path) = match overlay {
                Overlay::FileExplorer { cwd, .. } => parent_dir(cwd),
                Overlay::FileViewer { path, .. } => parent_dir(path),
                _ => None,
            } {
                return OverlayAction::FilePick { path, is_dir: true };
            }
            OverlayAction::None
        }
        Key::Right => OverlayAction::None,
        Key::Enter => match overlay {
            Overlay::Help => OverlayAction::BeginOnboarding,
            Overlay::StatusReport { .. } => OverlayAction::Close,
            Overlay::TurnLimit { .. } => OverlayAction::ContinueTurns,
            Overlay::ConnectModel {
                providers,
                provider_cursor,
                selected_route: _,
                all_items: _,
                groups,
                model_input,
                model_selected,
                effort_items,
                effort_selected,
                active_profile_id: _,
                active_model,
                active_effort: _,
                focus,
                catalog_loading: _,
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
                    if route.is_current {
                        // Already the active route — nothing to do.
                        return OverlayAction::Close;
                    }
                    // Providers is a standalone view now (no chained Models
                    // step) — picking a different connected route must fully
                    // resolve the session on its own: switch routes and land
                    // on a valid default model + effort, handled by the app.
                    OverlayAction::SwitchToRoute {
                        profile_id: route.profile_id.clone(),
                    }
                }
                ConnectModelColumn::Models => {
                    let filtered: Vec<&ModelGroup> = groups
                        .iter()
                        .filter(|g| group_matches_input(model_input, g))
                        .collect();
                    let rows = flatten_model_rows(&filtered);
                    // Every row already names one specific route (single-route
                    // groups render as one row; multi-route groups render one
                    // row per route) — no auto-pick needed, the selected row
                    // *is* the explicit route choice. `model_selected` is
                    // always reset to 0 on every input-changing keystroke and
                    // clamped on navigation, so `chosen_route` is `None` iff
                    // `filtered`/`rows` is genuinely empty — never merely
                    // because the typed substring differs from the row's full
                    // canonical id. Trust the resolved row whenever there is
                    // one; only fall back to free text when there truly is no
                    // catalog match at all (previously this compared the raw
                    // filter text against the full id, which almost never
                    // matched and silently discarded a correct selection —
                    // see `openai-codex/luna is not found` regression).
                    let chosen_route = rows
                        .get(*model_selected)
                        .and_then(|row| flat_row_item(&filtered, row));
                    let Some(route) = chosen_route else {
                        let typed = model_input.trim();
                        if !typed.is_empty() {
                            // Catalogs are the entitlement boundary. Do not
                            // turn arbitrary text into a model selection.
                            return OverlayAction::ModelNotInCatalog(typed.to_string());
                        }
                        return OverlayAction::None;
                    };
                    // Models is a standalone view now — the app applies the
                    // model, resolves a safe effort default for it, and
                    // closes the overlay. No in-place Effort hand-off.
                    OverlayAction::SelectModel {
                        provider: route.provider.clone(),
                        model: route.model.clone(),
                        profile_id: route.profile_id.clone(),
                    }
                }
                ConnectModelColumn::Effort => {
                    if !ReasoningEffort::model_supports_effort(active_model) {
                        // Nothing to select — the column is explanatory only.
                        OverlayAction::None
                    } else {
                        effort_items
                            .get(*effort_selected)
                            .copied()
                            .map(OverlayAction::SelectEffort)
                            .unwrap_or(OverlayAction::None)
                    }
                }
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
            Overlay::Theme {
                selected, items, ..
            } => items
                .get(*selected)
                .map(|(id, _)| OverlayAction::SelectTheme(id.clone()))
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
        Key::Char('y') | Key::Char('Y') if matches!(overlay, Overlay::TurnLimit { .. }) => {
            OverlayAction::ContinueTurns
        }
        Key::Char('n') | Key::Char('N') if matches!(overlay, Overlay::TurnLimit { .. }) => {
            OverlayAction::StopTurns
        }
        _ => OverlayAction::None,
    }
}

#[cfg(test)]
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
    if forge_governance::is_shell_tool(tool) {
        ApprovalExecutionMode::Shell
    } else {
        ApprovalExecutionMode::Direct
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
            .or_else(|| args.get("cmd"))
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

/// Bottom band used when `OverlayWidget` paints the theme picker into a full
/// frame (tests / fallback). Prefer the layout `input` region from `draw`.
/// Card in the lower-right of `area`. Use this when the host is already the
/// pane above the theme list (the conversation column), not the full frame.
pub fn theme_preview_card(area: Rect) -> Rect {
    if area.width < 24 || area.height < 8 {
        return Rect::new(area.x, area.y, 0, 0);
    }
    // Keep a left gutter so a conversation line is still readable.
    let reserved = 28.min(area.width.saturating_sub(24));
    let width = area
        .width
        .saturating_sub(reserved)
        .clamp(24, 58)
        .min(area.width);
    let height = area.height.min(16);
    Rect {
        x: area.x.saturating_add(area.width.saturating_sub(width)),
        y: area.y.saturating_add(area.height.saturating_sub(height)),
        width,
        height,
    }
}

/// Snippet card above the theme dock, right-aligned so the list stays readable.
pub fn theme_preview_rect(area: Rect) -> Rect {
    if area.width < 24 || area.height < 8 {
        return Rect::new(area.x, area.y, 0, 0);
    }
    let dock = theme_dock_rect(area);
    let gap = 1;
    let available = dock.y.saturating_sub(area.y).saturating_sub(gap);
    let height = available.clamp(8, 16);
    let max_w = area.width.saturating_sub(2);
    let width = (area.width.saturating_mul(58) / 100).max(24).min(max_w);
    let x = area
        .x
        .saturating_add(area.width.saturating_sub(width).saturating_sub(1));
    let y = dock
        .y
        .saturating_sub(height)
        .saturating_sub(gap)
        .max(area.y);
    Rect {
        x,
        y,
        width,
        height: height.min(dock.y.saturating_sub(y)).min(area.height),
    }
}

fn theme_dock_rect(area: Rect) -> Rect {
    let height = crate::layout::THEME_DOCK_H
        .min(area.height.saturating_sub(1))
        .max(3);
    Rect::new(
        area.x,
        area.y + area.height.saturating_sub(height),
        area.width,
        height,
    )
}

/// Render the live-preview theme dock into `area` (composer slot or bottom band).
pub fn render_theme_dock(
    selected: usize,
    current: &str,
    items: &[(String, String)],
    area: Rect,
    buf: &mut Buffer,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .style(theme::panel())
        .title(Span::styled(
            " Theme · ↑↓ preview · Enter confirm · Esc cancel ",
            theme::brand(),
        ));
    let inner = block.inner(area);
    block.render(area, buf);
    let list_area = inner;

    // System is always first; keep a separator under it when present.
    let mut rows: Vec<(usize, ListItem)> = Vec::with_capacity(items.len().saturating_add(1));
    for (index, (id, name)) in items.iter().enumerate() {
        let marker = if index == selected { "▶ " } else { "  " };
        let is_current = id == current;
        let selected_row = index == selected;
        let style = if selected_row {
            theme::focused_selection_style()
        } else {
            theme::text()
        };
        let base = format!("{marker}{name} ({id})");
        let item = if is_current {
            ListItem::new(Line::from(vec![
                Span::styled(base, style),
                Span::styled(" · current", theme::tag_style(selected_row)),
            ]))
        } else {
            ListItem::new(Span::styled(base, style))
        };
        rows.push((index, item));
        if index == 0 {
            rows.push((
                usize::MAX,
                ListItem::new(Span::styled(
                    "─".repeat(list_area.width as usize),
                    theme::border_muted(),
                )),
            ));
        }
    }

    let visible = list_area.height.max(1) as usize;
    let selected_row_pos = rows
        .iter()
        .position(|(index, _)| *index == selected)
        .unwrap_or(0);
    let start = selected_row_pos
        .saturating_add(1)
        .saturating_sub(visible)
        .min(rows.len().saturating_sub(visible));
    let list_items: Vec<ListItem> = rows
        .into_iter()
        .skip(start)
        .take(visible)
        .map(|(_, item)| item)
        .collect();
    List::new(list_items).render(list_area, buf);
}

pub struct OverlayWidget<'a> {
    pub overlay: &'a Overlay,
}

impl Widget for OverlayWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        match self.overlay {
            // Dim the transcript in place instead of blanking it, so it stays
            // legible-but-muted behind the picker rather than disappearing.
            // `Help` (aka `welcome()`) doubles as the zero-state onboarding
            // overlay auto-opened on a disconnected launch, which must keep
            // the conversation visible per the onboarding requirement — and
            // dimming is a strict improvement for the plain `/help` case too.
            Overlay::ConnectModel { .. } | Overlay::Help => theme::dim_region(area, buf),
            // Theme dock keeps the real UI painted undimmed so live preview is honest.
            Overlay::Theme { .. } => {}
            _ => theme::fill(area, buf, theme::canvas()),
        }
        match self.overlay {
            Overlay::Help => {
                let r = centered_rect(64, 58, area);
                Paragraph::new(
                    "Forge is an AI coding agent for your terminal.\n\nStart typing and press Enter.\n\nShortcuts\n• /       Commands\n• /status Session status\n• Tab / Shift+Tab  Focus visible blocks\n• Ctrl+`  Toggle bottom panel\n• Alt+M  Quick-switch model\n• Alt+, / Alt+.  Change effort\n• Footer chips: Enter opens/cycles the selected chip\n• ← / →  Switch tab in the active block\n• Enter/i Interact\n• Tab     Complete (Chat composer)\n• ↑↓      Navigate local list or input\n• Esc     Leave one interaction level\n• F1      Help\n\nEditor (when a text file is open)\n• Normal mode on open; i  Insert mode\n• :w / :q / :wq  Save / quit / save and quit\n• :e [path]  Reload or open a workspace file\n• :s/.../.../  Replace on the current line\n• :%s/.../.../  Replace across the buffer\n• Alt+E  Open the external editor\n• Esc     Return to workspace\n\nText files are editable. Binary and invalid-UTF-8 files are read-only. Forge\nasks before leaving dirty buffers and offers reload or force-save when disk\ncontent changed.\n\nForge asks before sensitive actions and automatically saves your session.\n\nPress Enter to get started.",
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
                catalog_loading,
                ..
            } => {
                let r = centered_capped_rect(area, 78, 29);
                // `dim_region` above only re-tones existing cell colors, it
                // doesn't clear glyphs — without an explicit blank here,
                // widgets that don't pad every cell to full width (like
                // `Table`) leave stray background characters showing through.
                theme::fill(r, buf, theme::panel());
                let active = active_vendor_route_labels(providers, active_profile_id.as_deref());
                let title_text = match focus {
                    ConnectModelColumn::Providers => "Select a provider",
                    ConnectModelColumn::Models => "Select a model",
                    ConnectModelColumn::Effort => "Select reasoning effort",
                };
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::border())
                    .style(theme::panel())
                    .padding(Padding::new(1, 1, 1, 0))
                    .title(Span::styled(format!(" {title_text} "), theme::brand()));
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
                let info_area = regions[0];
                let list_area = regions[1];

                match focus {
                    ConnectModelColumn::Providers => {
                        // No filter/info line for this view — the whole body is the list.
                        let flat = flatten_provider_rows(providers);
                        let visible = list_area.height.max(1) as usize;
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
                                    let target = (list_area.width as usize)
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
                        List::new(provider_items).render(list_area, buf);
                    }
                    ConnectModelColumn::Models => {
                        // Type-ahead filter line, then the (cross-route or
                        // scoped, per how this instance was opened) catalog.
                        let filter_text = if model_input.is_empty() {
                            format!("{}Type to filter models…", theme::CURSOR_GLYPH)
                        } else {
                            format!("{}{}", model_input, theme::CURSOR_GLYPH)
                        };
                        Paragraph::new(Line::from(vec![
                            Span::styled("⌕ ", theme::dim()),
                            Span::styled(
                                filter_text,
                                if model_input.is_empty() {
                                    theme::dim()
                                } else {
                                    theme::text()
                                },
                            ),
                        ]))
                        .render(info_area, buf);
                        let cursor_x = info_area.x.saturating_add(2).saturating_add(
                            if model_input.is_empty() {
                                0
                            } else {
                                model_input.chars().count() as u16
                            },
                        );
                        if cursor_x < info_area.right() {
                            theme::paint_caret(buf, cursor_x, info_area.y);
                        }

                        // A real Table so PROVIDER/SOURCE columns stay aligned
                        // no matter how long a MODEL or PROVIDER cell's text
                        // is — fixed string padding can't truncate overlong
                        // cells, which drifts every column after it.
                        let widths = [
                            Constraint::Min(20),
                            Constraint::Length(22),
                            Constraint::Length(16),
                        ];
                        let header = Row::new(vec![
                            Cell::from(Span::styled(
                                "MODEL",
                                theme::muted().add_modifier(Modifier::BOLD),
                            )),
                            Cell::from(Span::styled(
                                "PROVIDER",
                                theme::muted().add_modifier(Modifier::BOLD),
                            )),
                            Cell::from(Span::styled(
                                "SOURCE / ACCOUNT",
                                theme::muted().add_modifier(Modifier::BOLD),
                            )),
                        ]);

                        // Blank rows around the header (rendered as its own
                        // single-row table sharing `widths`/spacing with the
                        // body table below, so the columns still line up)
                        // give the list the breathing room a bare `Table`
                        // with `.header()` packs too tightly.
                        let sections = Layout::default()
                            .direction(Direction::Vertical)
                            .constraints([
                                Constraint::Length(1),
                                Constraint::Length(1),
                                Constraint::Length(1),
                                Constraint::Min(1),
                            ])
                            .split(list_area);
                        let header_area = sections[1];
                        let list_area = sections[3];
                        Table::new(vec![header], widths)
                            .column_spacing(1)
                            .render(header_area, buf);

                        let filtered: Vec<&ModelGroup> = groups
                            .iter()
                            .filter(|g| group_matches_input(model_input, g))
                            .collect();
                        let rows = flatten_model_rows(&filtered);
                        let visible = list_area.height.max(1) as usize;
                        let start = window_start_pin_top(*model_selected, rows.len(), visible);
                        let end = (start + visible).min(rows.len());
                        let table_rows: Vec<Row> = if rows.is_empty() {
                            let msg = if active_profile_id.is_none() {
                                "Connect a provider first."
                            } else if *catalog_loading {
                                "Loading models…"
                            } else if model_input.trim().is_empty() {
                                "No models available yet."
                            } else {
                                "No models match this filter."
                            };
                            vec![Row::new(vec![Cell::from(Span::styled(
                                msg,
                                theme::muted(),
                            ))])]
                        } else {
                            rows[start..end]
                                .iter()
                                .enumerate()
                                .map(|(i, row)| {
                                    let idx = start + i;
                                    let selected = idx == *model_selected;
                                    let row_style = if selected {
                                        theme::focused_selection_style()
                                    } else {
                                        theme::text()
                                    };
                                    let g = filtered[row.group_idx];
                                    let item = flat_row_item(&filtered, row)
                                        .expect("flatten_model_rows only emits valid indices");
                                    let is_current = item.model == *active_model;
                                    let source = if is_current {
                                        "current"
                                    } else {
                                        match item.source {
                                            forge_connect::CatalogSource::Registry => "known",
                                            _ => "cloud",
                                        }
                                    };
                                    let provider = if item.route_label.is_empty() {
                                        "·"
                                    } else {
                                        item.route_label.as_str()
                                    };
                                    let marker = if selected { "▶ " } else { "  " };
                                    Row::new(vec![
                                        Cell::from(format!("{marker}{}", g.model_id)),
                                        Cell::from(provider.to_string()),
                                        Cell::from(source),
                                    ])
                                    .style(row_style)
                                })
                                .collect()
                        };
                        Table::new(table_rows, widths)
                            .column_spacing(1)
                            .render(list_area, buf);
                    }
                    ConnectModelColumn::Effort => {
                        // No filter/info line for this view — the whole body
                        // is the list (or the non-configurable explanation).
                        // A model with no adjustable effort at all renders as
                        // an explicit, non-selectable explanation instead of
                        // a real one-item list — the control must not look
                        // actionable when there is nothing to choose.
                        let effort_list_items: Vec<ListItem> =
                            if !ReasoningEffort::model_supports_effort(active_model) {
                                vec![ListItem::new(Span::styled(
                                    "Effort is not configurable for this model.",
                                    theme::muted(),
                                ))]
                            } else {
                                let default_effort =
                                    ReasoningEffort::default_for_model(active_model);
                                effort_items
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
                                        let base =
                                            format!("{marker}{}{default_label}", effort.label());
                                        if is_current {
                                            ListItem::new(Line::from(vec![
                                                Span::styled(base, style),
                                                Span::styled(
                                                    " current",
                                                    theme::tag_style(selected),
                                                ),
                                            ]))
                                        } else {
                                            ListItem::new(Span::styled(base, style))
                                        }
                                    })
                                    .collect()
                            };
                        List::new(effort_list_items).render(list_area, buf);
                    }
                }

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
                let key_style = theme::panel_alt()
                    .fg(theme::text_primary_color())
                    .add_modifier(Modifier::BOLD);
                let label_style = theme::dim();
                Paragraph::new(Line::from(vec![
                    Span::styled(" ↑↓ ", key_style),
                    Span::styled(" Select   ", label_style),
                    Span::styled(" Enter ", key_style),
                    Span::styled(" Confirm   ", label_style),
                    Span::styled(" Esc ", key_style),
                    Span::styled(" Close", label_style),
                ]))
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
                let cursor_y = r.y + 7;
                let cursor_x = r.x + 1 + key_input.chars().count() as u16;
                if cursor_y < r.bottom() && cursor_x < r.right() {
                    theme::paint_caret(buf, cursor_x, cursor_y);
                }
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
                        let row = match &item.title {
                            Some(title) => format!("{marker}{title}  ·  {}", item.modified),
                            None => format!("{marker}{}  ·  {}", item.id, item.modified),
                        };
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
                render_theme_dock(*selected, current, items, theme_dock_rect(area), buf);
                if let Some((id, _)) = items.get(*selected) {
                    crate::theme_preview::render_theme_preview(id, theme_preview_rect(area), buf);
                }
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
    use forge_config::THEME_SOLARIZED_LIGHT;
    use ratatui::widgets::Widget;
    use serde_json::json;

    #[test]
    fn window_start_pin_top_puts_selected_on_the_first_row_when_room_allows() {
        // Plenty of room below `selected` to fill the window without
        // scrolling past the list's end — selected should be the top row.
        assert_eq!(window_start_pin_top(4, 20, 5), 4);
        // Selected is close enough to the end that pinning it to the top
        // would scroll past the list — clamp instead of overscrolling.
        assert_eq!(window_start_pin_top(18, 20, 5), 15);
        // Whole list already fits on screen — no scrolling at all.
        assert_eq!(window_start_pin_top(3, 4, 5), 0);
    }

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

    fn two_route_items() -> Vec<ModelItem> {
        vec![
            ModelItem {
                provider: "native".into(),
                model: "openai/gpt-5.6".into(),
                profile_id: Some("openai".into()),
                source: forge_connect::CatalogSource::Live,
                route_label: "OpenAI".into(),
            },
            ModelItem {
                provider: "native".into(),
                model: "anthropic/claude-sonnet-4-6".into(),
                profile_id: Some("anthropic".into()),
                source: forge_connect::CatalogSource::Live,
                route_label: "Anthropic".into(),
            },
        ]
    }

    /// Regression: `index_of_model` used to return an index into `groups`
    /// (one entry per unique model id) while the render path treats
    /// `model_selected` as an index into the *flattened* rows (one entry
    /// per route, so a multi-route group contributes more than one row).
    /// Any earlier multi-route group drifted the initial selection off the
    /// real active model — this reproduces that shape: two groups ahead of
    /// the active one each have two routes, so the flattened index is two
    /// slots ahead of the naive group index.
    #[test]
    fn initial_selection_accounts_for_earlier_multi_route_groups() {
        let items = vec![
            ModelItem {
                provider: "native".into(),
                model: "alpha".into(),
                profile_id: Some("opencode".into()),
                source: forge_connect::CatalogSource::Live,
                route_label: "OpenCode".into(),
            },
            ModelItem {
                provider: "native".into(),
                model: "alpha".into(),
                profile_id: Some("openai".into()),
                source: forge_connect::CatalogSource::Live,
                route_label: "OpenAI".into(),
            },
            ModelItem {
                provider: "native".into(),
                model: "beta".into(),
                profile_id: Some("opencode".into()),
                source: forge_connect::CatalogSource::Live,
                route_label: "OpenCode".into(),
            },
            ModelItem {
                provider: "native".into(),
                model: "beta".into(),
                profile_id: Some("openai".into()),
                source: forge_connect::CatalogSource::Live,
                route_label: "OpenAI".into(),
            },
            ModelItem {
                provider: "native".into(),
                model: "gamma".into(),
                profile_id: Some("opencode".into()),
                source: forge_connect::CatalogSource::Live,
                route_label: "OpenCode".into(),
            },
        ];
        let overlay = Overlay::connect_model_open(
            vec![],
            items,
            None,
            "gamma",
            ReasoningEffort::default(),
            ConnectModelColumn::Models,
        );
        let Overlay::ConnectModel { model_selected, .. } = &overlay else {
            panic!("expected ConnectModel overlay");
        };
        // Flattened rows are: alpha/OpenCode(0), alpha/OpenAI(1),
        // beta/OpenCode(2), beta/OpenAI(3), gamma/OpenCode(4) — "gamma" must
        // land on 4, not on group index 2.
        assert_eq!(*model_selected, 4);
    }

    #[test]
    fn compact_model_control_searches_across_all_connected_routes() {
        // Active profile is "openai" — the full-screen picker (a deliberate,
        // guided browse) should still start scoped to it...
        let full = Overlay::connect_model_open(
            vec![],
            two_route_items(),
            Some("openai"),
            "openai/gpt-5.6",
            ReasoningEffort::default(),
            ConnectModelColumn::Models,
        );
        let Overlay::ConnectModel {
            selected_route,
            groups,
            ..
        } = &full
        else {
            panic!("expected ConnectModel overlay");
        };
        assert_eq!(selected_route.as_deref(), Some("openai"));
        assert_eq!(
            groups.len(),
            1,
            "full-screen picker starts scoped to the active route"
        );

        // ...but the compact "model control" must search every connected
        // route by default, per "the model control opens a searchable list
        // built from every currently connected profile/route, not just the
        // active route."
        let compact = Overlay::connect_model_open_compact(
            vec![],
            two_route_items(),
            Some("openai"),
            "openai/gpt-5.6",
            ReasoningEffort::default(),
            ConnectModelColumn::Models,
        );
        let Overlay::ConnectModel {
            selected_route,
            groups,
            active_profile_id,
            ..
        } = &compact
        else {
            panic!("expected ConnectModel overlay");
        };
        assert_eq!(
            *selected_route, None,
            "compact control must not scope to the active route by default"
        );
        assert_eq!(
            groups.len(),
            2,
            "expected both connected routes' models to be searchable: {groups:?}"
        );
        // "current" tagging still reflects the real active profile even
        // though search itself is unscoped.
        assert_eq!(active_profile_id.as_deref(), Some("openai"));
    }

    #[test]
    fn model_search_matches_route_label_case_insensitively() {
        let mut overlay = Overlay::connect_model_open_compact(
            vec![],
            two_route_items(),
            None,
            "",
            ReasoningEffort::default(),
            ConnectModelColumn::Models,
        );
        for c in "ANTHRO".chars() {
            handle_overlay_key(&mut overlay, Key::Char(c));
        }
        let Overlay::ConnectModel {
            groups,
            model_input,
            ..
        } = &overlay
        else {
            panic!("expected ConnectModel overlay");
        };
        let matching: Vec<&ModelGroup> = groups
            .iter()
            .filter(|g| group_matches_input(model_input, g))
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "expected only the Anthropic route to match: {matching:?}"
        );
        assert_eq!(matching[0].model_id, "claude-sonnet-4-6");
    }

    #[test]
    fn compact_connect_model_overlay_renders_centered_same_as_full_screen() {
        let area = Rect::new(0, 0, 100, 48);

        let full = Overlay::connect_model_open(
            vec![],
            vec![],
            None,
            "",
            ReasoningEffort::default(),
            ConnectModelColumn::Models,
        );
        let compact = Overlay::connect_model_open_compact(
            vec![],
            vec![],
            None,
            "",
            ReasoningEffort::default(),
            ConnectModelColumn::Models,
        );

        let title_row = |overlay: &Overlay| -> u16 {
            let mut buf = Buffer::empty(area);
            OverlayWidget { overlay }.render(area, &mut buf);
            for y in 0..area.height {
                let mut line = String::new();
                for x in 0..area.width {
                    line.push_str(buf[(x, y)].symbol());
                }
                if line.contains("Select a model") {
                    return y;
                }
            }
            panic!("title not found");
        };

        let full_row = title_row(&full);
        let compact_row = title_row(&compact);
        assert_eq!(
            compact_row, full_row,
            "the footer-triggered compact picker should center on screen just like the full picker, not anchor near the bottom"
        );
    }

    fn sample_default_models() -> Vec<ModelItem> {
        let mut items = Vec::new();
        for p in forge_connect::loaded_registry().profiles() {
            let mut seen = std::collections::BTreeSet::new();
            for m in &p.default_models {
                if seen.insert(m.clone()) {
                    items.push(ModelItem {
                        provider: "native".into(),
                        model: m.clone(),
                        profile_id: Some(p.id.clone()),
                        source: forge_connect::CatalogSource::Default,
                        route_label: p.vendor_label.clone(),
                    });
                }
            }
        }
        items
    }

    /// The picker now shows exactly one view per instance (see
    /// `docs/provider-model-effort-modal-restructure.md`) — there is no
    /// longer a "focused vs unfocused column" distinction to test, since
    /// only one column is ever rendered at all. Its single visible list's
    /// cursor row always carries the selection highlight; covered by
    /// `model_select`/`single_route_model_still_selects_immediately_on_enter`
    /// and the render assertions in `overlay_widget_renders_model_empty_states`.
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

    fn hyphenated_model_item() -> ModelItem {
        ModelItem {
            provider: "native".into(),
            model: "openai-codex/gpt-5.6-luna".into(),
            profile_id: Some("openai_codex".into()),
            source: forge_connect::CatalogSource::Default,
            route_label: "OpenAI Codex".into(),
        }
    }

    /// Two providers offering the same model name.
    fn duplicate_name_items() -> Vec<ModelItem> {
        vec![
            ModelItem {
                provider: "native".into(),
                model: "opencode-zen/gpt-5.6-luna".into(),
                profile_id: Some("opencode_zen".into()),
                source: forge_connect::CatalogSource::Live,
                route_label: "OpenCode · Zen".into(),
            },
            ModelItem {
                provider: "native".into(),
                model: "openai-codex/gpt-5.6-luna".into(),
                profile_id: Some("openai_codex".into()),
                source: forge_connect::CatalogSource::Live,
                route_label: "OpenAI · ChatGPT".into(),
            },
        ]
    }

    #[test]
    fn typing_a_duplicated_model_name_keeps_the_provider_you_are_already_on() {
        // The picker resets the selection to row 0 on every keystroke, so
        // typing a name two providers offer used to confirm whichever route
        // sorted first — silently moving the session to a different provider
        // and account. Row 0 must be the "stay put" choice.
        let mut overlay = Overlay::connect_model_open_compact(
            vec![],
            duplicate_name_items(),
            Some("openai_codex"),
            "openai-codex/gpt-5.6-luna",
            ReasoningEffort::default(),
            ConnectModelColumn::Models,
        );
        for c in "gpt-5.6-luna".chars() {
            handle_overlay_key(&mut overlay, Key::Char(c));
        }
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::SelectModel {
                provider: "native".into(),
                model: "openai-codex/gpt-5.6-luna".into(),
                profile_id: Some("openai_codex".into()),
            }
        );
    }

    #[test]
    fn the_other_provider_is_still_selectable_after_promotion() {
        // Promotion reorders; it must never hide a route.
        let mut overlay = Overlay::connect_model_open_compact(
            vec![],
            duplicate_name_items(),
            Some("openai_codex"),
            "openai-codex/gpt-5.6-luna",
            ReasoningEffort::default(),
            ConnectModelColumn::Models,
        );
        for c in "gpt-5.6-luna".chars() {
            handle_overlay_key(&mut overlay, Key::Char(c));
        }
        handle_overlay_key(&mut overlay, Key::Down);
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::SelectModel {
                provider: "native".into(),
                model: "opencode-zen/gpt-5.6-luna".into(),
                profile_id: Some("opencode_zen".into()),
            }
        );
    }

    #[test]
    fn model_partial_filter_match_selects_the_highlighted_catalog_row_not_free_text() {
        // Regression for "openai-codex/luna is not found": typing a substring
        // of a real catalog id must confirm the already-highlighted row, not
        // discard it and re-dispatch the raw substring as free text.
        let mut overlay = model_overlay(vec![hyphenated_model_item()], ConnectModelColumn::Models);
        for c in "luna".chars() {
            handle_overlay_key(&mut overlay, Key::Char(c));
        }
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::SelectModel {
                provider: "native".into(),
                model: "openai-codex/gpt-5.6-luna".into(),
                profile_id: Some("openai_codex".into()),
            }
        );
    }

    #[test]
    fn model_no_catalog_match_is_rejected() {
        let mut overlay = model_overlay(vec![hyphenated_model_item()], ConnectModelColumn::Models);
        for c in "totally-unknown-model".chars() {
            handle_overlay_key(&mut overlay, Key::Char(c));
        }
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::ModelNotInCatalog("totally-unknown-model".into())
        );
    }

    #[test]
    fn model_empty_filter_selects_current_row_on_enter() {
        let mut overlay = model_overlay(vec![hyphenated_model_item()], ConnectModelColumn::Models);
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::SelectModel {
                provider: "native".into(),
                model: "openai-codex/gpt-5.6-luna".into(),
                profile_id: Some("openai_codex".into()),
            }
        );
    }

    #[test]
    fn model_rejects_typed_custom_model() {
        let mut overlay = model_overlay(sample_default_models(), ConnectModelColumn::Models);
        for c in "openai/custom-model".chars() {
            assert_eq!(
                handle_overlay_key(&mut overlay, Key::Char(c)),
                OverlayAction::None
            );
        }
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::ModelNotInCatalog("openai/custom-model".into())
        );
    }

    #[test]
    fn model_input_supports_paste_and_backspace() {
        let mut overlay = model_overlay(sample_default_models(), ConnectModelColumn::Models);
        handle_overlay_key(&mut overlay, Key::Paste("anthropic/custom-modelx\n".into()));
        handle_overlay_key(&mut overlay, Key::Backspace);
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::ModelNotInCatalog("anthropic/custom-model".into())
        );
    }

    #[test]
    fn resume_picker_moves_and_runs_selected_session() {
        let mut overlay = Overlay::resume_picker(vec![
            ResumeSessionItem {
                id: "first".into(),
                modified: "newest".into(),
                title: None,
            },
            ResumeSessionItem {
                id: "second".into(),
                modified: "older".into(),
                title: None,
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
    fn providers_column_enter_on_a_different_connected_route_switches_to_it() {
        // Providers is a standalone view now — picking a different,
        // already-connected route can't chain into Models in place (there's
        // no Models view open); it must hand off a complete, self-sufficient
        // action for the app layer to resolve a default model + effort for.
        let mut registry = forge_connect::ConnectRegistry::new();
        registry.register(forge_connect::anthropic_profile());
        registry.register(forge_connect::ollama_profile());
        let connected: std::collections::HashSet<String> =
            ["anthropic".to_string(), "ollama".to_string()]
                .into_iter()
                .collect();
        let providers = build_provider_rows(&registry, &connected, Some("ollama"));
        let mut overlay = Overlay::connect_model_open(
            providers,
            vec![],
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
            OverlayAction::SwitchToRoute {
                profile_id: "anthropic".into()
            }
        );
    }

    #[test]
    fn providers_column_enter_on_the_already_active_route_just_closes() {
        let mut registry = forge_connect::ConnectRegistry::new();
        registry.register(forge_connect::ollama_profile());
        let connected: std::collections::HashSet<String> =
            ["ollama".to_string()].into_iter().collect();
        let providers = build_provider_rows(&registry, &connected, Some("ollama"));
        let mut overlay = Overlay::connect_model_open(
            providers,
            vec![],
            Some("ollama"),
            "ollama/llama3",
            ReasoningEffort::default(),
            ConnectModelColumn::Providers,
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::Close,
            "re-picking the already-active route is a no-op close, not a route switch"
        );
    }

    #[test]
    fn tab_and_backtab_do_nothing_since_each_view_is_standalone() {
        // Each picker instance shows exactly one view for its whole
        // lifetime — there is nothing left for Tab/Shift-Tab to cycle
        // between (see `docs/provider-model-effort-modal-restructure.md`).
        let mut overlay = Overlay::connect_model_open(
            vec![],
            vec![],
            None,
            "",
            ReasoningEffort::default(),
            ConnectModelColumn::Providers,
        );
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Tab),
            OverlayAction::None
        );
        assert!(matches!(
            &overlay,
            Overlay::ConnectModel {
                focus: ConnectModelColumn::Providers,
                ..
            }
        ));
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::BackTab),
            OverlayAction::None
        );
        assert!(matches!(
            &overlay,
            Overlay::ConnectModel {
                focus: ConnectModelColumn::Providers,
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
                route_label: "OpenAI".into(),
            },
            ModelItem {
                provider: "native".into(),
                model: "openai/gpt-5.6".into(),
                profile_id: Some("openrouter".into()),
                source: forge_connect::CatalogSource::Live,
                route_label: "OpenRouter".into(),
            },
        ]
    }

    #[test]
    fn multi_route_group_renders_each_route_as_a_separate_selectable_row() {
        // A model offered by more than one connected route must never be
        // silently auto-resolved — each route renders as its own row, named
        // by its route label, so the choice is explicit.
        let overlay = model_overlay(shared_route_items(), ConnectModelColumn::Models);
        let text = render_text(&overlay);
        assert!(
            text.contains("OpenAI") && text.contains("OpenRouter"),
            "expected both routes' labels to appear as distinct rows:\n{text}"
        );

        // Row 0 is the first route (OpenAI); Enter selects it directly, no
        // auto-pick logic involved.
        let mut first = model_overlay(shared_route_items(), ConnectModelColumn::Models);
        assert_eq!(
            handle_overlay_key(&mut first, Key::Enter),
            OverlayAction::SelectModel {
                provider: "native".into(),
                model: "openai/gpt-5.6".into(),
                profile_id: Some("openai".into()),
            }
        );

        // Row 1 is the second route (OpenRouter); moving down and pressing
        // Enter selects *that exact route*, proving the choice is explicit
        // rather than always landing on one preferred source.
        let mut second = model_overlay(shared_route_items(), ConnectModelColumn::Models);
        second.move_sel(1);
        assert_eq!(
            handle_overlay_key(&mut second, Key::Enter),
            OverlayAction::SelectModel {
                provider: "native".into(),
                model: "openai/gpt-5.6".into(),
                profile_id: Some("openrouter".into()),
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
                route_label: "OpenAI".into(),
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
    fn effort_column_is_explanatory_and_non_actionable_for_an_unsupported_model() {
        let mut overlay = Overlay::connect_model_open(
            vec![],
            vec![],
            None,
            "openai/gpt-4.1-mini",
            ReasoningEffort::default(),
            ConnectModelColumn::Effort,
        );
        assert!(!ReasoningEffort::model_supports_effort(
            "openai/gpt-4.1-mini"
        ));

        let text = render_text(&overlay);
        assert!(
            text.contains("not configurable"),
            "expected an explanatory message:\n{text}"
        );
        assert!(!text.contains("(default)"));
        assert!(!text.contains("current"));

        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::None,
            "Enter must not dispatch a selection when there is nothing to choose"
        );
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
        crate::theme::install(
            crate::theme_registry::ThemeRegistry::load(None),
            forge_config::THEME_SOLARIZED_DARK,
        );
        let mut overlay = Overlay::theme_open(forge_config::THEME_SOLARIZED_DARK);
        let light_index = match &overlay {
            Overlay::Theme { items, .. } => items
                .iter()
                .position(|(id, _)| id == THEME_SOLARIZED_LIGHT)
                .expect("solarized-light in picker"),
            _ => panic!("expected theme overlay"),
        };
        if let Overlay::Theme { selected, .. } = &mut overlay {
            *selected = light_index;
        }
        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Enter),
            OverlayAction::SelectTheme(THEME_SOLARIZED_LIGHT.to_string())
        );
    }

    #[test]
    fn theme_picker_arrow_keys_preview_highlighted_theme() {
        crate::theme::install(
            crate::theme_registry::ThemeRegistry::load(None),
            forge_config::THEME_SOLARIZED_DARK,
        );
        let mut overlay = Overlay::theme_open(forge_config::THEME_SOLARIZED_DARK);
        let Overlay::Theme {
            selected, items, ..
        } = &overlay
        else {
            panic!("expected theme overlay");
        };
        let start = *selected;
        let next_id = items[(start + 1) % items.len()].0.clone();

        assert_eq!(
            handle_overlay_key(&mut overlay, Key::Down),
            OverlayAction::PreviewTheme(next_id)
        );
        assert!(matches!(overlay, Overlay::Theme { .. }));
    }

    #[test]
    fn theme_dock_renders_at_bottom_without_blanking_title_chrome() {
        crate::theme::install(
            crate::theme_registry::ThemeRegistry::load(None),
            forge_config::THEME_SOLARIZED_DARK,
        );
        let overlay = Overlay::theme_open(forge_config::THEME_SOLARIZED_DARK);
        let text = render_text(&overlay);
        assert!(
            text.contains("Theme · ↑↓ preview · Enter confirm · Esc cancel"),
            "expected live-preview dock chrome:\n{text}"
        );
        assert!(
            text.contains("· current"),
            "expected current-theme marker:\n{text}"
        );
        assert!(
            text.contains("Preview"),
            "expected theme preview pane:\n{text}"
        );
        assert!(
            text.contains("What does this project do?"),
            "expected composer suggestion in preview:\n{text}"
        );
        assert!(
            text.contains("approval"),
            "expected approval snippet in preview:\n{text}"
        );
        // Dock sits in the bottom band — title should appear in the lower half.
        let area = Rect::new(0, 0, 100, 48);
        let mut buf = Buffer::empty(area);
        OverlayWidget { overlay: &overlay }.render(area, &mut buf);
        let mut title_row = None;
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            if row.contains("Theme ·") {
                title_row = Some(y);
                break;
            }
        }
        let title_row = title_row.expect("theme dock title");
        assert!(
            title_row > area.height / 2,
            "theme dock should sit in the bottom half (row {title_row})"
        );
    }

    #[test]
    fn picker_selection_wraps_in_both_directions() {
        let mut overlay = Overlay::resume_picker(vec![
            ResumeSessionItem {
                id: "one".into(),
                modified: "now".into(),
                title: None,
            },
            ResumeSessionItem {
                id: "two".into(),
                modified: "then".into(),
                title: None,
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
        assert!(help.contains("← / →"));

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
    fn overlay_widget_renders_model_empty_states() {
        let empty_model = render_text(&model_overlay(vec![], ConnectModelColumn::Models));
        assert!(empty_model.contains("Connect a provider first."));

        let mut filtered_model = Overlay::connect_model_open(
            vec![],
            vec![ModelItem {
                provider: "native".into(),
                model: "openai/gpt-5".into(),
                profile_id: Some("openai".into()),
                source: forge_connect::CatalogSource::Registry,
                route_label: "OpenAI".into(),
            }],
            Some("openai"),
            "",
            ReasoningEffort::default(),
            ConnectModelColumn::Models,
        );
        handle_overlay_key(&mut filtered_model, Key::Char('z'));
        let text = render_text(&filtered_model);
        assert!(text.contains("No models match this filter."));

        let empty_catalog_model = Overlay::connect_model_open(
            vec![],
            vec![],
            Some("openai"),
            "",
            ReasoningEffort::default(),
            ConnectModelColumn::Models,
        );
        let text = render_text(&empty_catalog_model);
        assert!(text.contains("No models available yet."));

        let mut loading_model = empty_catalog_model;
        if let Overlay::ConnectModel {
            catalog_loading, ..
        } = &mut loading_model
        {
            *catalog_loading = true;
        }
        let text = render_text(&loading_model);
        assert!(text.contains("Loading models…"));
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
        assert!(picker.contains("Select a provider"));
        assert!(picker.contains("Ollama"));
        assert!(picker.contains("current"));
        assert!(picker.contains("xAI"));

        let resume = render_text(&Overlay::resume_picker(vec![ResumeSessionItem {
            id: "session-123".into(),
            modified: "2026-07-29 05:00".into(),
            title: None,
        }]));
        assert!(resume.contains("Resume a session"));
        assert!(resume.contains("session-123"));
    }

    #[test]
    fn resume_picker_shows_title_hint_when_present_instead_of_raw_id() {
        let resume = render_text(&Overlay::resume_picker(vec![ResumeSessionItem {
            id: "session-123".into(),
            modified: "2026-07-29 05:00".into(),
            title: Some("fix the login bug".into()),
        }]));
        assert!(resume.contains("fix the login bug"), "{resume}");
        assert!(!resume.contains("session-123"), "{resume}");
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

#[cfg(test)]
mod request_view_tests {
    use super::*;

    /// The transcript renders `ApprovalRequestView`, so the payload-to-command
    /// reduction is covered here rather than in `conversation` — which no
    /// longer knows how a `HitlPayload` becomes a command line.
    #[test]
    fn shell_payload_reduces_to_its_command_line() {
        let view = ApprovalOverlayState::request_view(
            &HitlPayload {
                call_id: "1".into(),
                tool: "bash".into(),
                args_redacted: serde_json::json!({"command": "git push -u origin feature"}),
                reason: "policy requires human approval".into(),
                sandbox_escalation: false,
            },
            "workspace",
        );
        assert_eq!(view.tool, "bash");
        assert_eq!(view.command, "git push -u origin feature");
        assert_eq!(view.cwd, "workspace");
    }

    /// A direct-execution tool joins executable and arguments instead.
    #[test]
    fn direct_payload_joins_executable_and_arguments() {
        let view = ApprovalOverlayState::request_view(
            &HitlPayload {
                call_id: "2".into(),
                tool: "write_file".into(),
                args_redacted: serde_json::json!({"path": "a.txt", "content": "hi"}),
                reason: "policy".into(),
                sandbox_escalation: false,
            },
            "wd",
        );
        assert_eq!(view.tool, "write_file");
        assert!(
            !view.command.is_empty(),
            "a direct-mode tool must still describe what would run"
        );
    }
}
