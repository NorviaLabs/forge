//! Provider connection, credentials and model selection for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Covers the `/connect` flow end to end: the
//! picker, API-key entry, the OAuth device dance and its polling, credential
//! persistence, and the model and reasoning-effort selection that follows a
//! successful connection. Methods are moved verbatim.

use super::*;
use crate::overlays::build_provider_rows_with_reuse;

/// Provider connection/auth state: known profiles, stored credentials, the
/// active profile selection, and in-flight xAI OAuth polling. #19 phase 3 —
/// the first extracted sub-model; grouped verbatim from six `TuiApp` fields,
/// no behavior change.
struct ModelCostCache {
    model: String,
    cost: Option<forge_connect::CatalogCost>,
}

pub(crate) struct ConnectionModel {
    pub(super) registry: ConnectRegistry,
    /// The process credentials handle (store, session keys, refresh).
    pub(super) credentials: std::sync::Arc<forge_connect::Credentials>,
    /// Non-secret interactive selections, in their own file.
    pub(super) preferences: PreferenceStore,
    pub(super) profile: Option<String>,
    /// Manual disconnect latch: prevents auto-restore until the user signs in again.
    pub(super) auth_suspended: bool,
    /// In-flight xAI device-code OAuth (polled on the event loop tick).
    pub(super) oauth_pending: Option<OauthPending>,
    /// Last time we polled the token endpoint (respect server `interval`).
    pub(super) oauth_last_poll: Option<std::time::Instant>,
    /// Cached answer to "are credentials live for the active profile", with
    /// the moment it was taken.
    ///
    /// Deciding this walks every registered profile and reads the credential
    /// file for each, and every read stats that file. Asking per frame put
    /// seven syscalls on the render path. The value only changes when the user
    /// connects or disconnects — which invalidates it explicitly — or when the
    /// file is edited outside Forge, which the TTL catches.
    pub(super) connected: Option<(std::time::Instant, bool)>,
    model_cost_cache: Option<ModelCostCache>,
    catalog_errors: std::collections::HashMap<String, String>,
    auth_required: HashSet<String>,
}

impl ConnectionModel {
    pub(super) fn new() -> Self {
        Self {
            registry: loaded_registry(),
            credentials: forge_connect::Credentials::shared(),
            preferences: PreferenceStore::user_default(),
            profile: None,
            auth_suspended: false,
            oauth_pending: None,
            oauth_last_poll: None,
            connected: None,
            model_cost_cache: None,
            catalog_errors: Default::default(),
            auth_required: Default::default(),
        }
    }
}

impl TuiApp {
    pub(super) fn ensure_model_credentials(&mut self, profile_id: &str) -> bool {
        if self.connect.auth_required.contains(profile_id) || !self.credentials_live_for(profile_id)
        {
            if needs_tui_oauth(&self.connect.registry, profile_id) {
                self.begin_oauth_flow(profile_id);
            } else {
                self.open_api_key_prompt(profile_id, Some("Authentication required".into()));
            }
            false
        } else {
            true
        }
    }
    /// Mock provider is always "connected" (offline tests / CI).
    fn is_mock_provider(&self) -> bool {
        self.runtime.provider.eq_ignore_ascii_case("mock")
            || self.runtime.model_label.eq_ignore_ascii_case("mock")
    }

    /// Report a catalog refresh outcome to the connect service, the only
    /// writer of the persisted `auth_required` flag.
    fn report_catalog_auth(&self, profile_id: &str, failed: bool) {
        let svc = ConnectService {
            registry: &self.connect.registry,
            credentials: &self.connect.credentials,
            preferences: &self.connect.preferences,
            active_profile_id: self.connect.profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        let _ = if failed {
            svc.report_auth_failure(profile_id)
        } else {
            svc.report_auth_success(profile_id)
        };
    }

    /// Live credentials still exist for a connect profile id.
    fn credentials_live_for(&self, profile_id: &str) -> bool {
        let svc = ConnectService {
            registry: &self.connect.registry,
            credentials: &self.connect.credentials,
            preferences: &self.connect.preferences,
            active_profile_id: self.connect.profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        svc.connected_profiles()
            .ok()
            .map(|ps| ps.iter().any(|p| p.id == profile_id))
            .unwrap_or(false)
    }

    /// How long a cached connection answer is trusted before it is taken
    /// again. Bounds how stale the header chip can be when the credential file
    /// is edited outside Forge; every in-app change invalidates it directly.
    const CONNECTED_TTL: std::time::Duration = std::time::Duration::from_secs(2);

    /// True when chat may call an LLM (mock, or a live `/connect` profile).
    ///
    /// Reads the credential file, so it is not for the render path — use
    /// [`Self::poll_connected`] and the cached value there.
    pub fn is_provider_connected(&self) -> bool {
        if self.is_mock_provider() {
            return true;
        }
        match self.connect.profile.as_deref() {
            Some(id) => self.credentials_live_for(id),
            None => false,
        }
    }

    /// Startup needs both live credentials and a model selection before the
    /// normal composer is usable.
    pub(super) fn needs_provider_model_setup(&self) -> bool {
        !self.is_provider_connected() || self.selected_model_label().trim().is_empty()
    }

    /// The render path's answer to "is the provider connected".
    ///
    /// Only the credential lookup is cached — the mock and no-profile checks
    /// are cheap, and they depend on `runtime.provider`, which can change
    /// without touching credentials. Caching those too meant a provider switch
    /// kept reading as connected until the TTL lapsed.
    pub(super) fn connected_cached(&mut self) -> bool {
        if self.is_mock_provider() {
            return true;
        }
        let Some(id) = self.connect.profile.clone() else {
            return false;
        };
        if let Some((at, live)) = self.connect.connected {
            if at.elapsed() < Self::CONNECTED_TTL {
                return live;
            }
        }
        let live = self.credentials_live_for(&id);
        self.connect.connected = Some((std::time::Instant::now(), live));
        live
    }

    /// Read the connection status populated by the event-loop tick. This must
    /// remain side-effect free because status construction is part of drawing.
    pub(super) fn provider_connected_cached(&self) -> bool {
        self.is_mock_provider()
            || (self.connect.profile.is_some()
                && self
                    .connect
                    .connected
                    .is_some_and(|(_, connected)| connected))
    }

    /// Drop the cached answer after anything that could change it.
    pub(super) fn invalidate_connected(&mut self) {
        self.connect.connected = None;
    }

    /// Vendor and offering labels for `profile_id`. The offering is always
    /// returned so identical model IDs remain distinguishable across routes.
    pub(super) fn vendor_route_labels(&self, profile_id: &str) -> (Option<String>, Option<String>) {
        let Some(profile) = self.connect.registry.get(profile_id) else {
            return (None, None);
        };
        let route = (!profile.label.is_empty()).then(|| profile.label.clone());
        (Some(profile.provider_label.clone()), route)
    }

    /// Drop a stale `connect.profile` if credentials were cleared out-of-band.
    fn sync_provider_connection(&mut self) {
        if self.connect.auth_suspended {
            return;
        }
        if self.is_mock_provider() {
            return;
        }
        if let Some(id) = self.connect.profile.clone() {
            if !self.credentials_live_for(&id) {
                self.connect.profile = None;
            }
        }
    }

    /// Status/input/banner chrome reflecting connect state.
    pub(super) fn apply_connection_chrome(mut self) -> Self {
        self.refresh_connection_ui();
        self
    }

    pub(super) fn refresh_connection_ui(&mut self) {
        self.sync_provider_connection();
        self.refresh_connection_chrome();
    }

    /// Bring the composer hint and the not-connected banner in line with the
    /// current profile, without second-guessing which profile is active.
    /// Used after a deliberate route/model pick, where dropping the profile
    /// the user just chose would be wrong.
    pub(super) fn refresh_connection_chrome(&mut self) {
        // This runs after sign-in, sign-out, and profile switches, so whatever
        // was cached describes the state before the change.
        self.invalidate_connected();
        let connected = self.is_provider_connected();
        self.input.not_connected = !connected;
        if connected {
            if self.input.hint.contains("Not connected") || self.input.hint.contains("/connect") {
                self.input.hint = String::new();
            }
            // Drop the sticky not-connected banner once signed in.
            self.banner_state.items.retain(|b| {
                !matches!(
                    b,
                    ChatItem::Banner {
                        kind: BannerKind::Warn,
                        text
                    } if text.contains("Not connected")
                )
            });
        } else {
            self.input.hint = self.disconnected_message();
        }
    }

    /// One message for every "you need to connect first" surface — the
    /// input hint, a direct-send attempt, and a queued-message dequeue
    /// attempt all read this instead of each hardcoding their own wording
    /// (previously three near-duplicate strings, one of them naming
    /// `xAI Grok`/`OpenCode Go` even when neither was configured).
    pub(super) fn disconnected_message(&self) -> String {
        "Not connected · run /connect to choose a provider".into()
    }

    pub(super) fn disconnect_auth(&mut self, profile_id: Option<&str>) -> Result<String, TuiError> {
        // Disconnect never changes the process environment; a profile whose
        // key comes from env stays usable, and the message says how to stop it.
        let env_var =
            profile_id.and_then(|id| forge_connect::winning_env_var(&self.connect.registry, id));
        self.connect.oauth_pending = None;
        self.connect.oauth_last_poll = None;
        self.pending_turn.clear();
        self.pending_interaction.clear();
        if let Some(mut handle) = self.pending_approved_tool.take() {
            handle.abort();
        }
        // The future-task queue is durable session state (owned by
        // `AgentSession`, not the TUI) — a provider disconnect must not
        // silently drop queued instructions.
        self.task_selection.clear_queue();
        self.stream.clear_preview();
        self.stream.thinking.clear();
        self.timing.started = None;
        self.timing.turn_started = None;
        self.timing.thinking_started = None;
        self.timing.thought_secs = None;
        self.cancellation.clear();
        self.busy_state.stop();
        self.tool_detail.collapse();
        self.conversation_view.follow = true;
        self.conversation_view.scroll = 0;
        self.connect.profile = None;
        self.runtime.provider.clear();
        self.runtime.model_label.clear();
        match self.selected_runtime() {
            SelectedRuntime::Direct => {
                if let Some(session) = self.session_runtime.as_mut() {
                    session.set_active_model(String::new());
                }
            }
            SelectedRuntime::Supervised(session_id) => {
                self.try_session_command(forge_session::SupervisorCommand::SetModel {
                    session_id,
                    model_id: String::new(),
                    route_id: String::new(),
                    reasoning_effort: Some(self.reasoning_effort.value.to_string()),
                });
            }
        }
        self.sync_model_capabilities();
        self.feedback = FeedbackModel::default();
        self.status_state.message = "disconnected".into();
        self.banner_state.items.retain(|b| {
            !matches!(
                b,
                ChatItem::Banner {
                    kind: BannerKind::Warn,
                    text
                } if text.contains("Not connected")
            )
        });
        self.connect.auth_suspended = true;

        let cleared = if let Some(id) = profile_id {
            match self.connect.registry.get(id) {
                Some(profile) => self.connect.credentials.clear(profile),
                None => self.connect.credentials.store().clear(id),
            }
            .map_err(|e| TuiError::Other(e.to_string()))?
        } else {
            self.connect
                .credentials
                .clear_all()
                .map_err(|e| TuiError::Other(e.to_string()))?
        };
        if let Some(id) = profile_id {
            let _ = self.connect.preferences.clear_last_selection(Some(id));
        } else {
            let _ = self.connect.preferences.clear_last_selection(None);
        }
        self.refresh_connection_ui();
        let msg = if let (Some(id), Some(var)) = (profile_id, env_var.as_deref()) {
            format!("`{id}` is set by `${var}`; unset it to disconnect")
        } else if let Some(id) = profile_id {
            if cleared {
                format!("disconnected `{id}`")
            } else {
                format!("no stored credentials for `{id}`")
            }
        } else if cleared {
            "disconnected · cleared stored credentials".into()
        } else {
            "disconnected · no stored credentials".into()
        };
        self.push_activity(ActivityKind::Connect, FeedbackSeverity::Info, msg.clone());
        self.set_feedback(FeedbackSeverity::Info, msg.clone());
        Ok(msg)
    }

    /// Reload credentials from disk: silent OAuth refresh, inject auth, activate profile.
    /// So a successful `/connect` continues to work in later Forge sessions.
    pub(super) fn restore_saved_auth(mut self) -> Self {
        let svc = ConnectService {
            registry: &self.connect.registry,
            credentials: &self.connect.credentials,
            preferences: &self.connect.preferences,
            active_profile_id: None,
            active_model: None,
        };
        let mut connected = svc.connected_profiles().unwrap_or_default();
        connected.sort_by(|a, b| a.id.cmp(&b.id));
        // Effort and the model/profile selection are persisted (and restored)
        // independently — a session can have a saved effort with no saved
        // model/profile yet (e.g. before ever connecting), so this
        // deliberately doesn't gate on `last_selection_struct()`, which would
        // only return `Some` when a complete selection exists.
        let saved_selection = self.connect.preferences.last_selection().ok().flatten();
        // The saved profile can be stale relative to the saved model: a model
        // id is namespaced by its provider (`opencode-go/x`), so when the two
        // disagree the model is authoritative. Otherwise a stale profile pairs
        // a foreign model with the wrong route and the resumed session cannot
        // answer a single turn.
        let saved_profile_id = saved_selection.as_ref().map(|(profile_id, model)| {
            self.connect
                .registry
                .profile_for_model(model)
                .map(|spec| spec.id.clone())
                .unwrap_or_else(|| profile_id.clone())
        });
        if let Some(effort) = self
            .connect
            .preferences
            .last_effort()
            .ok()
            .flatten()
            .and_then(|effort| effort.parse().ok())
        {
            self.reasoning_effort.value = effort;
        }
        // Restore the last usable provider; otherwise fall back to a deterministic
        // connected profile instead of silently preferring one backend family.
        let chosen = connected
            .iter()
            .find(|p| saved_profile_id.as_deref() == Some(p.id.as_str()))
            .cloned();
        if let Some(profile) = chosen {
            // Refresh a stale login now; requests read credentials per call.
            let _ = svc.ensure_oauth_fresh(&profile.id);
            self.connect.profile = Some(profile.id.clone());
            // The route decides the *transport*, and `transport_for_route(None)`
            // falls back to OpenAI-compat. Restoring the model without the route
            // therefore sends an Anthropic or Codex profile's requests over the
            // wrong wire, and every call fails until the user re-picks the model
            // — which is the only other path that sets it. The route follows the
            // profile, not the model, so it is restored either way below.
            let restored_route =
                forge_connect::route_id_for_profile(&self.connect.registry, &profile.id);
            match self.selected_runtime() {
                SelectedRuntime::Direct => {
                    if let Some(session) = self.session_runtime.as_mut() {
                        session.set_active_route_id(restored_route.clone());
                    }
                }
                SelectedRuntime::Supervised(session_id) => {
                    let active_model = self
                        .selected_details()
                        .map(|details| details.active_model.clone())
                        .unwrap_or_else(|| self.runtime.model_label.clone());
                    // Never persist a route without a model: an empty model id
                    // under a real route is the exact "model id is required"
                    // state. The selection block below applies a valid model.
                    if !active_model.trim().is_empty() {
                        self.try_session_command(forge_session::SupervisorCommand::SetModel {
                            session_id,
                            model_id: active_model,
                            route_id: restored_route,
                            reasoning_effort: Some(self.reasoning_effort.value.to_string()),
                        });
                    }
                }
            }
            // Only switch the active model when it still looks like the forge default
            // (don't clobber an explicit --model / test runtime label).
            let cur = self.runtime.model_label.as_str();
            let looks_default =
                cur.is_empty() || cur == "openai/gpt-4.1-mini" || cur == "m" || cur == "mock";
            if looks_default {
                let saved_model = saved_selection
                    .as_ref()
                    .map(|(_, model)| model.as_str())
                    .filter(|model| self.profile_owns_model(&profile, model));
                if let Some(model) = saved_model.or_else(|| profile.default_model()) {
                    self.apply_selection(&ModelSelection {
                        route_id: self
                            .connect
                            .profile
                            .as_deref()
                            .map(|id| {
                                forge_connect::route_id_for_profile(&self.connect.registry, id)
                            })
                            .unwrap_or_default(),
                        provider: "native".into(),
                        model: model.to_string(),
                        profile_id: Some(profile.id.clone()),
                        effort: self.reasoning_effort.value.to_string(),
                    });
                }
            } else {
                let active_model_empty = match self.selected_runtime() {
                    SelectedRuntime::Direct => self
                        .session_runtime
                        .as_ref()
                        .is_none_or(|session| session.active_model.is_empty()),
                    SelectedRuntime::Supervised(_) => self
                        .selected_details()
                        .is_none_or(|details| details.active_model.is_empty()),
                };
                if active_model_empty {
                    let candidate = self.runtime.model_label.clone();
                    let model = if self.profile_owns_model(&profile, &candidate) {
                        candidate
                    } else {
                        profile.default_model().unwrap_or_default().to_string()
                    };
                    if model.is_empty() {
                        // No model this profile can run: keep the profile but
                        // never write an empty model id under its route.
                        self.connect.profile = Some(profile.id.clone());
                    } else {
                        self.apply_selection(&ModelSelection {
                            route_id: forge_connect::route_id_for_profile(
                                &self.connect.registry,
                                &profile.id,
                            ),
                            provider: "native".into(),
                            model,
                            profile_id: Some(profile.id.clone()),
                            effort: self.reasoning_effort.value.to_string(),
                        });
                    }
                }
            }
            self.status_state.message =
                format!("restored {} · {}", profile.id, self.runtime.model_label);
        }
        self
    }

    /// After applying a model switch from the picker, resolve reasoning effort
    /// for the new model:
    /// falls back silently to the provider default when the previous effort
    /// isn't supported, persisting that fallback immediately since there's no
    /// picker open to commit it on close. Returns whether the model offers a
    /// real effort choice worth surfacing.
    pub(super) fn resolve_effort_for_model(&mut self, model: &str) -> bool {
        let options = ReasoningEffort::options_for_model(model);
        let default = ReasoningEffort::default_for_model(model);
        let previous = self.reasoning_effort.value;
        if !options.contains(&previous) {
            self.reasoning_effort.value = default;
            self.persist_selection();
            self.note_unsupported_effort_fallback(previous, default);
        }
        options.len() > 1
    }

    /// Spec'd copy for switching to a model that can't run the current
    /// effort: "{old} effort is not supported by this model. Using {new}."
    fn note_unsupported_effort_fallback(
        &mut self,
        previous: ReasoningEffort,
        fallback: ReasoningEffort,
    ) {
        self.set_feedback(
            FeedbackSeverity::Warn,
            format!(
                "{} effort is not supported by this model. Using {}.",
                previous.label(),
                fallback.label()
            ),
        );
    }

    /// Push the current effort selection into the session as the transport
    /// value the active model actually supports, or `None` when the model
    /// doesn't support effort at all (never send a meaningless value).
    /// Recomputed fresh every turn (see `drain_pending_prompt`) from
    /// `reasoning_effort.value` + `runtime.model_label`, which persistence,
    /// restore, quick-switch, and picker-close all keep current — so this
    /// can't drift out of sync with whichever model is actually active.
    pub(super) fn sync_effort_to_session(&mut self) {
        let supports = ReasoningEffort::model_supports_effort(&self.runtime.model_label);
        let value = supports
            .then(|| self.reasoning_effort.value.transport_value())
            .filter(|v| !v.is_empty())
            .map(str::to_string);
        if self.selected_is_supervised() {
            self.try_session_command(forge_session::SupervisorCommand::SetModel {
                session_id: self.selected_session_id,
                model_id: self.runtime.model_label.clone(),
                route_id: self.selected_active_route_id(),
                reasoning_effort: value,
            });
            self.try_session_command(forge_session::SupervisorCommand::SetThinking {
                session_id: self.selected_session_id,
                enabled: self.thinking_enabled,
            });
            return;
        }
        self.session_runtime.set_reasoning_effort(value);
        self.session_runtime
            .set_thinking_enabled(self.thinking_enabled);
    }

    /// Re-read the active model's cached models.dev metadata: whether it
    /// accepts image input, and the context/output token limits compaction
    /// sizes itself from. Network refreshes run through `start_catalog_refresh`.
    ///
    /// The single place `runtime.provider`, `runtime.model_label`,
    /// `session.active_model`, and `connect.profile` are set together (effort
    /// only when the selection carries a parseable one) — every model/route
    /// change must go through this rather than assigning the fields by hand,
    /// so the copies can't drift the way the picker's Enter-key bug once let
    /// a discarded selection produce a mismatched model id.
    pub(super) fn sync_model_capabilities(&mut self) {
        let model = match self.selected_runtime() {
            SelectedRuntime::Direct => self
                .session_runtime
                .as_ref()
                .map(|session| session.active_model.clone())
                .unwrap_or_else(|| self.runtime.model_label.clone()),
            SelectedRuntime::Supervised(_) => self
                .selected_details()
                .map(|details| details.active_model.clone())
                .filter(|model| !model.is_empty())
                .unwrap_or_else(|| self.runtime.model_label.clone()),
        };
        let cache = forge_connect::ModelCatalogCache::user_default();
        let supported = cache.model_accepts_image_input(&model);
        let output = cache
            .model_limits(&model)
            .and_then(|limits| (limits.output > 0).then_some(limits.output));
        match self.selected_runtime() {
            SelectedRuntime::Direct => {
                if let Some(session) = self.session_runtime.as_mut() {
                    session.set_image_input_supported(supported);
                    session.set_context_window(500_000, output);
                }
            }
            SelectedRuntime::Supervised(session_id) => {
                self.try_session_command(forge_session::SupervisorCommand::SetCapabilities {
                    session_id,
                    image_input_supported: supported,
                    context_window: Some((500_000, output)),
                });
            }
        }
    }

    pub(super) fn apply_selection(&mut self, selection: &ModelSelection) {
        self.runtime.provider = if selection.provider.trim().is_empty() {
            "native".into()
        } else {
            selection.provider.clone()
        };
        self.runtime.model_label = selection.model.clone();
        if let Ok(effort) = selection.effort.parse::<ReasoningEffort>() {
            self.reasoning_effort.value = effort;
        }
        match self.selected_runtime() {
            SelectedRuntime::Direct => {
                if let Some(session) = self.session_runtime.as_mut() {
                    session.set_active_model(&selection.model);
                    session.set_active_route_id(&selection.route_id);
                    session.set_reasoning_effort(Some(self.reasoning_effort.value.to_string()));
                }
            }
            SelectedRuntime::Supervised(session_id) => {
                self.try_session_command(forge_session::SupervisorCommand::SetModel {
                    session_id,
                    model_id: selection.model.clone(),
                    route_id: selection.route_id.clone(),
                    reasoning_effort: Some(self.reasoning_effort.value.to_string()),
                });
            }
        }
        self.sync_model_capabilities();
        self.connect.profile = selection.profile_id.clone();
    }

    pub(super) fn persist_selection(&self) {
        let model = self.runtime.model_label.trim();
        // Persist the profile the model actually belongs to; `connect.profile`
        // can lag behind a model switch and writing the two independently is
        // what left a stale profile paired with a foreign model on disk.
        let profile_id = self
            .connect
            .registry
            .profile_for_model(model)
            .map(|spec| spec.id.clone())
            .or_else(|| self.connect.profile.clone());
        if !model.is_empty() {
            if let Some(profile_id) = profile_id.as_deref() {
                let _ = self
                    .connect
                    .preferences
                    .set_last_selection(profile_id, model);
            }
        }
        let _ = self
            .connect
            .preferences
            .set_last_effort(&self.reasoning_effort.value.to_string());
    }

    /// Record a deliberate model/route/effort switch for Quick Switch.
    ///
    /// Unlike `persist_selection`, this rotates the prior selection into the
    /// Quick Switch history — call only from user-driven selection
    /// completion (a picker pick, a typed `/model`, an effort pick), never
    /// from automatic fallbacks or on shell exit.
    pub(super) fn record_deliberate_selection(&self) {
        let Some(profile_id) = self.connect.profile.clone() else {
            return;
        };
        if profile_id.trim().is_empty() || self.runtime.model_label.trim().is_empty() {
            return;
        }
        let _ = self.connect.preferences.record_switch((
            &profile_id,
            &self.runtime.model_label,
            &self.reasoning_effort.value.to_string(),
        ));
    }

    /// Toggle to the previously, deliberately selected model/route/effort
    /// combo — no picker, applies immediately at session scope.
    pub(super) fn quick_switch_model(&mut self) {
        match self.connect.preferences.quick_switch() {
            Ok(Some((profile_id, model, effort))) => {
                self.connect.auth_suspended = false;
                self.apply_selection(&ModelSelection {
                    route_id: forge_connect::route_id_for_profile(
                        &self.connect.registry,
                        &profile_id,
                    ),
                    provider: "native".into(),
                    model: model.clone(),
                    profile_id: Some(profile_id.clone()),
                    effort,
                });
                self.feedback = FeedbackModel::default();
                self.status_state.message.clear();
                self.push_activity(
                    ActivityKind::System,
                    FeedbackSeverity::Ok,
                    format!("quick switch → {model}"),
                );
            }
            Ok(None) => {
                self.set_feedback(FeedbackSeverity::Info, "no previous model to switch to");
            }
            Err(_) => {
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    "could not read the saved model selection",
                );
            }
        }
    }

    /// Build the unified Connect + Model + Effort picker: one state source
    /// for both `/connect` and `/model`, differing only in `focus`.
    /// `compact` selects the persistent footer control's small, anchored
    /// rendering instead of the full-screen browsing experience.
    pub(super) fn build_connect_model_overlay(
        &self,
        focus: ConnectModelColumn,
        compact: bool,
    ) -> Overlay {
        let connected: HashSet<String> = {
            let svc = ConnectService {
                registry: &self.connect.registry,
                credentials: &self.connect.credentials,
                preferences: &self.connect.preferences,
                active_profile_id: self.connect.profile.clone(),
                active_model: Some(self.runtime.model_label.clone()),
            };
            svc.connected_profiles()
                .unwrap_or_default()
                .into_iter()
                .map(|p| p.id)
                .collect()
        };
        let providers = build_provider_rows_with_reuse(
            &self.connect.registry,
            &connected,
            self.connect.profile.as_deref(),
            &self.reusable_logins(),
        );
        // Cache-only: instant open, never blocks on network I/O. A
        // background refresh (`start_catalog_refresh`, triggered by the
        // caller) updates these rows in place once it lands.
        let items = self.model_picker_items(false);
        let open = if compact {
            Overlay::connect_model_open_compact
        } else {
            Overlay::connect_model_open
        };
        open(
            providers,
            items,
            self.connect.profile.as_deref(),
            &self.runtime.model_label,
            self.reasoning_effort.value,
            focus,
        )
    }

    pub(super) fn open_connect_picker(&mut self) {
        self.status_state.message = "Choose a provider".into();
        self.reload_provider_registry();
        self.overlay = Some(self.build_connect_model_overlay(ConnectModelColumn::Providers, false));
    }

    pub(super) fn reload_provider_registry(&mut self) {
        let registry = loaded_registry();
        // `providers.toml` was edited outside Forge: the session's routing
        // snapshot follows, so a provider the picker shows is one requests
        // can reach. Unchanged definitions rebuild nothing.
        if registry.profiles() != self.connect.registry.profiles() {
            self.reload_providers();
        }
        self.connect.registry = registry;
        self.invalidate_connected();
        for warning in self.connect.registry.load_warnings().to_vec() {
            self.status_state.message = warning.clone();
            self.set_feedback(FeedbackSeverity::Error, warning.clone());
            self.push_activity(
                ActivityKind::Connect,
                FeedbackSeverity::Error,
                warning.clone(),
            );
            self.push_toast(warning);
        }
    }

    /// Open the persistent footer control's compact picker, focused on
    /// `focus`, without disturbing the conversation underneath.
    pub(super) fn open_connect_picker_compact(&mut self, focus: ConnectModelColumn) {
        self.reload_provider_registry();
        self.overlay = Some(self.build_connect_model_overlay(focus, true));
        self.refresh_open_picker_items();
        if focus == ConnectModelColumn::Models {
            self.start_catalog_refresh();
        }
    }

    pub(super) fn open_api_key_prompt(&mut self, profile_id: &str, error: Option<String>) {
        let p = self.connect.registry.get(profile_id);
        let title = p
            .map(|x| x.title.clone())
            .unwrap_or_else(|| profile_id.to_string());
        let auth_url = p.and_then(|x| x.auth_url.clone());
        let env_hint = if error.is_none() {
            p.and_then(|x| {
                x.api_key_env.iter().find_map(|env_name| {
                    std::env::var(env_name)
                        .ok()
                        .filter(|value| !value.is_empty())
                        .map(|_| env_name.clone())
                })
            })
        } else {
            None
        };
        let mut overlay = Overlay::connect_api_key(profile_id, title, auth_url, env_hint);
        if let Overlay::ConnectApiKey {
            error: overlay_error,
            ..
        } = &mut overlay
        {
            *overlay_error = error;
        }
        self.overlay = Some(overlay);
        self.status_state.message = format!("Connect {profile_id}");
    }

    /// Authentication does not select a model. The user confirms a model separately.
    pub(super) fn finish_connect_flow(&mut self, profile_id: &str) {
        self.connect.auth_required.remove(profile_id);
        self.invalidate_connected();
        self.open_model_picker_after_connect(profile_id);
    }

    fn connected_profile_count(&self) -> usize {
        let svc = ConnectService {
            registry: &self.connect.registry,
            credentials: &self.connect.credentials,
            preferences: &self.connect.preferences,
            active_profile_id: self.connect.profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        svc.connected_profiles().map(|v| v.len()).unwrap_or(0)
    }

    pub(super) fn open_model_picker_after_connect(&mut self, profile_id: &str) {
        self.reload_provider_registry();
        if self.connect.auth_required.contains(profile_id) {
            if needs_tui_oauth(&self.connect.registry, profile_id) {
                self.begin_oauth_flow(profile_id);
            } else {
                self.open_api_key_prompt(profile_id, Some("Authentication required".into()));
            }
            return;
        }
        let overlay = self.build_connect_model_overlay_scoped(
            ConnectModelColumn::Models,
            false,
            Some(profile_id),
        );
        self.overlay = Some(overlay);
        self.refresh_open_picker_items();
        let title = self
            .connect
            .registry
            .get(profile_id)
            .map(|p| p.title.as_str())
            .unwrap_or(profile_id);
        self.set_feedback(
            FeedbackSeverity::Ok,
            format!("{title} connected · choose a model"),
        );
        self.start_catalog_refresh();
    }

    fn build_connect_model_overlay_scoped(
        &self,
        focus: ConnectModelColumn,
        compact: bool,
        route_scope: Option<&str>,
    ) -> Overlay {
        let connected: HashSet<String> = {
            let svc = ConnectService {
                registry: &self.connect.registry,
                credentials: &self.connect.credentials,
                preferences: &self.connect.preferences,
                active_profile_id: self.connect.profile.clone(),
                active_model: Some(self.runtime.model_label.clone()),
            };
            svc.connected_profiles()
                .unwrap_or_default()
                .into_iter()
                .map(|p| p.id)
                .collect()
        };
        let providers = build_provider_rows_with_reuse(
            &self.connect.registry,
            &connected,
            self.connect.profile.as_deref(),
            &self.reusable_logins(),
        );
        let items = self.model_picker_items(false);
        if compact {
            return Overlay::connect_model_open_compact(
                providers,
                items,
                self.connect.profile.as_deref(),
                &self.runtime.model_label,
                self.reasoning_effort.value,
                focus,
            );
        }
        Overlay::connect_model_open_scoped(
            providers,
            items,
            self.connect.profile.as_deref(),
            route_scope,
            &self.runtime.model_label,
            self.reasoning_effort.value,
            focus,
        )
    }

    pub(super) fn handle_connect(&mut self, action: ConnectAction) {
        // /connect → interactive profile picker
        match &action {
            ConnectAction::Open | ConnectAction::List => {
                self.open_connect_picker();
                return;
            }
            _ => {}
        }

        // Phase 6.1: open mode-specific overlays for interactive connect
        let connect_target = if let ConnectAction::Connect {
            ref profile_id,
            ref api_key,
            oauth_fixture,
        } = action
        {
            if api_key.is_none() && !oauth_fixture {
                if needs_tui_api_key_prompt(&self.connect.registry, profile_id) {
                    // Existing file/env credentials should reconnect without
                    // asking the user to paste the same secret again.
                    if !self.credentials_live_for(profile_id) {
                        self.open_api_key_prompt(profile_id, None);
                        return;
                    }
                }
                if needs_tui_oauth(&self.connect.registry, profile_id) {
                    self.begin_oauth_flow(profile_id);
                    return;
                }
            }
            Some(profile_id.clone())
        } else {
            None
        };

        let mut model = Some(self.runtime.model_label.clone());
        match handle_connect_action(
            action,
            &self.connect.registry,
            &self.connect.credentials,
            &self.connect.preferences,
            &mut self.connect.profile,
            &mut model,
        ) {
            Ok(msg) => {
                if connect_target.is_none() {
                    if let Some(m) = model {
                        self.runtime.model_label = m.clone();
                        self.runtime.provider = "native".into();
                        self.connect.auth_suspended = false;
                        self.set_selected_model(m.to_string());
                        self.sync_model_capabilities();
                    }
                }
                let lines: Vec<String> = msg.lines().map(|s| s.to_string()).collect();
                self.status_state.message = lines.first().cloned().unwrap_or_default();
                self.set_feedback(FeedbackSeverity::Info, self.status_state.message.clone());
                self.push_activity(
                    ActivityKind::Connect,
                    FeedbackSeverity::Ok,
                    self.status_state.message.clone(),
                );
                if let Some(line) = lines.first() {
                    self.push_toast(line.clone());
                }
                self.refresh_connection_ui();
                if let Some(profile_id) = connect_target {
                    self.finish_connect_flow(&profile_id);
                }
            }
            Err(ConnectError::OauthDevicePending(pending)) => {
                // Payload is boxed so `ConnectError` stays small in every `Result`.
                self.show_oauth_pending(*pending);
            }
            Err(e) => {
                if matches!(&e, ConnectError::Verify(error) if error.is_auth_failure()) {
                    if let Some(id) = &connect_target {
                        self.connect.auth_required.insert(id.clone());
                    }
                }
                let error = e.to_string();
                if let Some(profile_id) =
                    connect_target.filter(|id| needs_tui_api_key_prompt(&self.connect.registry, id))
                {
                    self.open_api_key_prompt(&profile_id, Some(error));
                } else {
                    self.status_state.message = error.clone();
                    self.set_feedback(FeedbackSeverity::Error, error);
                }
            }
        }
    }

    pub(super) fn begin_oauth_flow(&mut self, profile_id: &str) {
        // A declared env key beats a stored login (design E3b); say so, so
        // signing in is not mistaken for what the requests use.
        if let Some(var) = forge_connect::winning_env_var(&self.connect.registry, profile_id) {
            self.set_feedback(
                FeedbackSeverity::Warn,
                format!("env key `${var}`; login unused"),
            );
        }
        let mut svc = ConnectService {
            registry: &self.connect.registry,
            credentials: &self.connect.credentials,
            preferences: &self.connect.preferences,
            active_profile_id: self.connect.profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        match svc.connect_start_oauth(profile_id) {
            Ok(Ok(out)) => {
                self.on_connect_success(&out);
            }
            Ok(Err(pending)) => self.show_oauth_pending(pending),
            Err(e) => {
                self.status_state.message = e.to_string();
                self.set_feedback(FeedbackSeverity::Error, e.to_string());
                self.report_error(&e.to_string());
            }
        }
    }

    /// After a successful connect: update model, inject credentials, clear OAuth UI.
    fn on_connect_success(&mut self, out: &forge_connect::ConnectOutcome) {
        self.connect.oauth_pending = None;
        self.connect.oauth_last_poll = None;
        self.refresh_connection_ui();
        self.finish_connect_flow(&out.profile_id);
    }

    /// Rebuild the routing snapshot after a provider definition change (a
    /// custom profile, a user override, or a manual model was saved).
    pub(super) fn reload_providers(&mut self) {
        self.connect.registry = loaded_registry();
        match self.selected_runtime() {
            SelectedRuntime::Direct => {
                if let Some(session) = self.session_runtime.as_mut() {
                    session.set_route_table(forge_session::route_table());
                }
            }
            SelectedRuntime::Supervised(_) => {
                self.try_session_command(forge_session::SupervisorCommand::ReloadProviders);
            }
        }
    }

    pub(super) fn show_oauth_pending(&mut self, pending: OauthPending) {
        let title = self
            .connect
            .registry
            .get(&pending.profile_id)
            .map(|p| p.title.clone())
            .unwrap_or_else(|| pending.profile_id.clone());
        let instructions = pending.operator_instructions();
        self.status_state.message = instructions
            .lines()
            .next()
            .map(str::to_string)
            .unwrap_or_else(|| format!("OAuth for {}", pending.profile_id));
        self.overlay = Some(Overlay::connect_oauth(
            pending.profile_id.clone(),
            title,
            instructions,
        ));
        self.connect.oauth_pending = Some(pending);
        self.connect.oauth_last_poll = None;
    }

    /// Poll device-code OAuth once (called from the TUI tick loop).
    pub fn poll_oauth_tick(&mut self) {
        let Some(pending) = self.connect.oauth_pending.clone() else {
            return;
        };
        let interval = Duration::from_secs(pending.interval_secs.max(1));
        if let Some(last) = self.connect.oauth_last_poll {
            if last.elapsed() < interval {
                return;
            }
        }
        self.connect.oauth_last_poll = Some(std::time::Instant::now());
        let mut svc = ConnectService {
            registry: &self.connect.registry,
            credentials: &self.connect.credentials,
            preferences: &self.connect.preferences,
            active_profile_id: self.connect.profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        match svc.poll_oauth_once(&pending) {
            Ok(Some(out)) => {
                self.on_connect_success(&out);
            }
            Ok(None) => {
                // still waiting
            }
            Err(e) => {
                self.connect.oauth_pending = None;
                self.connect.oauth_last_poll = None;
                self.overlay = None;
                self.report_error(&e.to_string());
            }
        }
    }

    pub(super) fn finish_connect(
        &mut self,
        profile_id: &str,
        api_key: Option<String>,
        oauth_fixture: bool,
    ) {
        self.handle_connect(ConnectAction::Connect {
            profile_id: profile_id.into(),
            api_key,
            oauth_fixture,
        });
    }

    /// Submit API key (or env) from the connect modal. On failure, keep the modal open
    /// with an error so the operator can re-paste (does not clear a long key on length checks
    /// when the failure came from Use-env short key).
    pub(super) fn try_connect_api_key(&mut self, profile_id: &str, api_key: Option<String>) {
        let saved_overlay = self.overlay.take();
        let mut model = Some(self.runtime.model_label.clone());
        let action = ConnectAction::Connect {
            profile_id: profile_id.into(),
            api_key: api_key.clone(),
            oauth_fixture: false,
        };
        match handle_connect_action(
            action,
            &self.connect.registry,
            &self.connect.credentials,
            &self.connect.preferences,
            &mut self.connect.profile,
            &mut model,
        ) {
            Ok(msg) => {
                // The probe accepted the key: hold it for this process too,
                // so it wins over env and the store until disconnect.
                if let (Some(key), Some(profile)) =
                    (api_key.as_deref(), self.connect.registry.get(profile_id))
                {
                    self.connect.credentials.set_session_key(profile, key);
                }
                self.status_state.message = msg.lines().next().unwrap_or_default().to_string();
                self.refresh_connection_ui();
                self.finish_connect_flow(profile_id);
                // Saved after a probe that neither accepted nor rejected it.
                if msg.contains("not verified") {
                    self.set_feedback(
                        FeedbackSeverity::Warn,
                        "key saved · not verified (the provider did not confirm it)",
                    );
                }
            }
            Err(e) => {
                let err = e.to_string();
                self.overlay = saved_overlay;
                if let Some(Overlay::ConnectApiKey { error, .. }) = &mut self.overlay {
                    *error = Some(err.clone());
                }
                self.status_state.message = err;
                self.push_activity(
                    ActivityKind::Connect,
                    FeedbackSeverity::Error,
                    format!("connect {profile_id} failed"),
                );
            }
        }
    }

    /// Apply a provider/model id to this session (no restart required).
    ///
    /// `profile_id`, when known, names the exact connect profile the caller
    /// picked (e.g. from route disambiguation in the picker) and is applied
    /// directly. Without it, the profile is re-derived from the model
    /// string's prefix — retained only for internal callers that do not have
    /// an explicit catalog route, and ambiguous once routes share prefixes.
    pub(super) fn apply_model_selection(
        &mut self,
        provider: &str,
        model: &str,
        profile_id: Option<&str>,
    ) {
        let model = model.trim();
        if model.is_empty() {
            return;
        }
        self.connect.auth_suspended = false;
        let resolved_profile_id = match profile_id {
            Some(profile_id) => Some(profile_id.to_string()),
            None => {
                // Match the selected model to its connected profile even when a
                // different provider was active before opening the picker.
                let prefix = model.split('/').next().unwrap_or("");
                let svc = ConnectService {
                    registry: &self.connect.registry,
                    credentials: &self.connect.credentials,
                    preferences: &self.connect.preferences,
                    active_profile_id: None,
                    active_model: None,
                };
                svc.connected_profiles().ok().and_then(|connected| {
                    connected
                        .iter()
                        .find(|p| p.model_provider_prefix == prefix || p.id == prefix)
                        .map(|p| p.id.clone())
                })
            }
        };
        // Effort is untouched here — the caller resolves it separately (see
        // `resolve_effort_for_model`), so carry the current value through
        // unchanged rather than resetting it.
        self.apply_selection(&ModelSelection {
            route_id: resolved_profile_id
                .as_deref()
                .map(|id| forge_connect::route_id_for_profile(&self.connect.registry, id))
                .unwrap_or_default(),
            provider: provider.to_string(),
            model: model.to_string(),
            profile_id: resolved_profile_id,
            effort: self.reasoning_effort.value.to_string(),
        });
        // Picking a model on an already-connected route is a connect too: the
        // composer hint and banner must follow, not only fresh sign-ins.
        self.refresh_connection_chrome();
        self.record_deliberate_selection();
        self.feedback = FeedbackModel::default();
        self.status_state.message.clear();
        self.push_activity(
            ActivityKind::System,
            FeedbackSeverity::Ok,
            format!("model {}", self.runtime.model_label),
        );
    }

    pub(super) fn model_prefix(model: &str) -> &str {
        model.split('/').next().unwrap_or("").trim()
    }

    /// True when `model` belongs to `profile`. The namespaced provider prefix
    /// on the model id is authoritative; un-namespaced ids fall back to the
    /// profile id / provider prefix comparison.
    pub(super) fn profile_owns_model(
        &self,
        profile: &forge_connect::ConnectProfile,
        model: &str,
    ) -> bool {
        if let Some(spec) = self.connect.registry.profile_for_model(model) {
            return spec.id == profile.id;
        }
        let prefix = Self::model_prefix(model);
        !prefix.is_empty() && (prefix == profile.id || prefix == profile.model_provider_prefix)
    }

    /// Build `/model` picker rows from connected-profile catalogs (cache + optional refresh).
    /// Every profile the picker/catalog worker should consider: connected
    /// routes, or every built-in default when nothing is connected yet.
    /// Shared by `model_picker_items` (foreground, cache-only reads) and
    /// `start_catalog_refresh` (background thread) so the two can never
    /// disagree about which profiles are in scope.
    fn picker_profiles(&self) -> Vec<forge_connect::ConnectProfile> {
        let svc = ConnectService {
            registry: &self.connect.registry,
            credentials: &self.connect.credentials,
            preferences: &self.connect.preferences,
            active_profile_id: self.connect.profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        let mut connected = svc.connected_profiles().unwrap_or_default();
        if let Some(id) = self.picker_route() {
            if !connected.iter().any(|profile| profile.id == id) {
                if let Some(profile) = self.connect.registry.get(&id) {
                    connected.push(profile.clone());
                }
            }
        }
        if connected.is_empty() {
            self.connect.registry.profiles().to_vec()
        } else {
            connected
        }
    }

    pub(super) fn model_picker_items(
        &self,
        refresh_stale: bool,
    ) -> Vec<crate::overlays::ModelItem> {
        // Normal model switching is account-scoped. Do not surface stale
        // caches from every built-in profile before a route is connected.
        if self.connected_profile_count() == 0 && self.picker_route().is_none() {
            return Vec::new();
        }
        let profiles = self.picker_profiles();
        let cache = ModelCatalogCache::user_default();
        let entries = runnable_models_for_picker(
            &profiles,
            self.connect.credentials.store(),
            &cache,
            refresh_stale,
        );
        let mut items = models_from_catalog(&entries);
        for item in &mut items {
            if let Some(profile_id) = item.profile_id.as_deref() {
                item.route_label = self.format_route_label(profile_id);
            }
        }
        items
    }

    /// "Vendor" or "Vendor · Route" display string for `profile_id`, matching
    /// the persistent footer control's formatting convention. Used to
    /// annotate each `ModelItem` so the model search can match against
    /// vendor/route text, not just the bare model id.
    fn format_route_label(&self, profile_id: &str) -> String {
        let (vendor, route) = self.vendor_route_labels(profile_id);
        match (vendor, route) {
            (Some(vendor), Some(route)) => format!("{vendor} · {route}"),
            (Some(vendor), None) => vendor,
            _ => String::new(),
        }
    }

    pub(super) fn picker_route(&self) -> Option<String> {
        self.overlay.as_ref().and_then(Overlay::picker_route)
    }

    pub(super) fn start_catalog_refresh(&mut self) {
        let Some(id) = self.picker_route() else {
            return;
        };
        if !self.credentials_live_for(&id) {
            return;
        }
        if self.catalog_fetch.refresh_rx.is_some() {
            if self.catalog_fetch.refreshing_profile.as_deref() != Some(&id) {
                self.catalog_fetch.pending_profile = Some(id);
            }
            self.refresh_open_picker_items();
            return;
        }
        let Some(profile) = self.connect.registry.get(&id).cloned() else {
            return;
        };
        let store_path = self.connect.credentials.store().path().to_path_buf();
        let cache_path = ModelCatalogCache::user_default().path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let store = CredentialStore::new(store_path);
            let cache = ModelCatalogCache::new(cache_path);
            let result =
                forge_connect::refresh_profile_catalog(&profile, &store, &cache).map(|_| ());
            let _ = tx.send(result);
        });
        self.catalog_fetch.refresh_rx = Some(rx);
        self.catalog_fetch.refreshing_profile = Some(id.clone());
        self.connect.catalog_errors.remove(&id);
        if let Some(overlay) = &mut self.overlay {
            overlay.set_catalog_error(None);
            overlay.set_catalog_loading(true);
        }
    }

    pub(super) fn poll_catalog_refresh(&mut self) {
        let Some(rx) = self.catalog_fetch.refresh_rx.take() else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.catalog_fetch.refresh_rx = Some(rx);
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(forge_connect::CatalogError::Message(
                    "Model refresh stopped; Ctrl-R to retry".into(),
                ))
            }
        };
        if let Some(id) = self.catalog_fetch.refreshing_profile.take() {
            match result {
                Ok(()) => {
                    self.connect.catalog_errors.remove(&id);
                    self.connect.auth_required.remove(&id);
                    self.report_catalog_auth(&id, false);
                    if self.connect.profile.as_deref() == Some(&id)
                        && !self.model_picker_items(false).iter().any(|item| {
                            item.profile_id.as_deref() == Some(&id)
                                && item.model == self.runtime.model_label
                        })
                    {
                        self.connect.catalog_errors.insert(
                            id,
                            "Active model: No longer listed · session unchanged".into(),
                        );
                    }
                }
                Err(error) => {
                    let message = if error.is_auth_failure() {
                        self.connect.auth_required.insert(id.clone());
                        if error.rejected_credential() {
                            self.report_catalog_auth(&id, true);
                        }
                        self.invalidate_connected();
                        "Authentication required · select the provider to retry or edit credentials"
                            .to_string()
                    } else if matches!(error.status(), Some(404 | 405 | 501)) {
                        "Discovery unsupported · Ctrl-A to add a model manually".to_string()
                    } else {
                        format!("Model refresh failed: {error} · Ctrl-R retry / Ctrl-A add model")
                    };
                    self.connect.catalog_errors.insert(id, message);
                }
            }
        }
        self.sync_model_capabilities();
        self.refresh_open_picker_items();
        if let Some(pending) = self.catalog_fetch.pending_profile.take() {
            if self.picker_route().as_deref() == Some(&pending) {
                self.start_catalog_refresh();
            }
        }
    }

    fn refresh_open_picker_items(&mut self) {
        if !matches!(self.overlay, Some(Overlay::ConnectModel { .. })) {
            return;
        }
        let route = self.picker_route();
        let error = route
            .as_ref()
            .and_then(|id| self.connect.catalog_errors.get(id))
            .cloned();
        let loading = self.catalog_fetch.refresh_rx.is_some()
            && self.catalog_fetch.refreshing_profile == route;
        let items = self.model_picker_items(false);
        if let Some(overlay) = &mut self.overlay {
            overlay.refresh_model_items(items);
            overlay.set_catalog_loading(loading);
            overlay.set_catalog_error(error);
        }
    }

    #[allow(dead_code)]
    fn active_model_cost(&mut self) -> Option<forge_connect::CatalogCost> {
        self.connect.active_model_cost(&self.runtime.model_label)
    }
}

impl ConnectionModel {
    fn active_model_cost(&mut self, model: &str) -> Option<forge_connect::CatalogCost> {
        if let Some(cache) = &self.model_cost_cache {
            if cache.model == model {
                return cache.cost;
            }
        }
        let cost = ModelCatalogCache::user_default().get_registry_cost(model);
        self.model_cost_cache = Some(ModelCostCache {
            model: model.to_string(),
            cost,
        });
        cost
    }
}

impl TuiApp {
    /// Sign-ins other CLIs on this machine already hold, that Forge could
    /// reuse instead of sending the user through a second browser grant.
    ///
    /// Read fresh each time the picker opens rather than cached at startup:
    /// signing in to `codex` in another terminal while Forge is running should
    /// show up here, and the read is three small files.
    pub(super) fn reusable_logins(&self) -> Vec<forge_connect::DiscoveredLogin> {
        let Some(home) = dirs::home_dir() else {
            return Vec::new();
        };
        forge_connect::discover_logins_in(&home, forge_connect::xdg_data_home().as_deref())
    }

    /// Adopt a discovered login for `profile_id`, if one exists.
    ///
    /// Returns whether it was adopted, so the caller can fall through to the
    /// ordinary browser/API-key flow when it was not. Only ever called for a
    /// row the user chose while it was labelled `reuse …` — this is not a
    /// silent import.
    pub(super) fn try_reuse_login(&mut self, profile_id: &str) -> bool {
        let Some(login) = forge_connect::login_for_profile(&self.reusable_logins(), profile_id)
        else {
            return false;
        };
        let stored = match &login.secret {
            forge_connect::DiscoveredSecret::ApiKey(key) => {
                match self.connect.registry.get(profile_id) {
                    Some(profile) => self.connect.credentials.set_api_key(profile, key),
                    None => self
                        .connect
                        .credentials
                        .store()
                        .set_api_key(profile_id, key),
                }
            }
            forge_connect::DiscoveredSecret::Oauth(tokens) => {
                match self.connect.registry.get(profile_id) {
                    Some(profile) => self
                        .connect
                        .credentials
                        .set_oauth(profile, (**tokens).clone()),
                    None => self
                        .connect
                        .credentials
                        .store()
                        .set_oauth(profile_id, (**tokens).clone()),
                }
            }
        };
        if let Err(error) = stored {
            // Fall back to the normal flow rather than stranding the user:
            // a failed import is an inconvenience, not a dead end.
            self.set_feedback(
                FeedbackSeverity::Warn,
                format!("Could not reuse the {} sign-in: {error}", login.source),
            );
            return false;
        }
        self.invalidate_connected();
        self.refresh_connection_ui();
        self.status_state.message = format!("Reused the {} sign-in", login.source);
        self.push_activity(
            ActivityKind::Connect,
            FeedbackSeverity::Ok,
            // `login` redacts its own secret, but say nothing about it here
            // either — this line reaches the transcript.
            format!("reuse {} sign-in for {profile_id}", login.source),
        );
        self.finish_connect_flow(profile_id);
        true
    }
}
