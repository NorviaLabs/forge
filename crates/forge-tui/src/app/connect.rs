//! Provider connection, credentials and model selection for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Covers the `/connect` flow end to end: the
//! picker, API-key entry, the OAuth device dance and its polling, credential
//! persistence, and the model and reasoning-effort selection that follows a
//! successful connection. Methods are moved verbatim.

use super::*;

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
    pub(super) store: CredentialStore,
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
}

impl ConnectionModel {
    pub(super) fn new() -> Self {
        Self {
            registry: loaded_registry(),
            store: CredentialStore::user_default(),
            preferences: PreferenceStore::user_default(),
            profile: None,
            auth_suspended: false,
            oauth_pending: None,
            oauth_last_poll: None,
            connected: None,
            model_cost_cache: None,
        }
    }
}

impl TuiApp {
    /// Mock provider is always "connected" (offline tests / CI).
    fn is_mock_provider(&self) -> bool {
        self.runtime.provider.eq_ignore_ascii_case("mock")
            || self.runtime.model_label.eq_ignore_ascii_case("mock")
    }

    /// Live credentials still exist for a connect profile id.
    fn credentials_live_for(&self, profile_id: &str) -> bool {
        let svc = ConnectService {
            registry: &self.connect.registry,
            store: &self.connect.store,
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
        let route = (!profile.route_label.is_empty()).then(|| profile.route_label.clone());
        (Some(profile.vendor_label.clone()), route)
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
        let mut env_keys = Vec::new();
        {
            let svc = ConnectService {
                registry: &self.connect.registry,
                store: &self.connect.store,
                preferences: &self.connect.preferences,
                active_profile_id: self.connect.profile.clone(),
                active_model: Some(self.runtime.model_label.clone()),
            };
            let profiles: Vec<_> = if let Some(id) = profile_id {
                svc.profile(id).into_iter().cloned().collect()
            } else {
                svc.connected_profiles().unwrap_or_default()
            };
            for p in profiles {
                if let Ok(pairs) = svc.provider_env_for_profile(&p.id) {
                    env_keys.extend(pairs.into_iter().map(|(k, _)| k));
                }
            }
        }
        for key in env_keys {
            std::env::remove_var(key);
        }
        self.session.clear_provider_env();
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
        self.session.set_active_model(String::new());
        self.sync_model_capabilities();
        self.feedback = FeedbackModel::default();
        self.status_state.message = "disconnected".into();
        self.notice_state.items.clear();
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
            self.connect
                .store
                .clear(id)
                .map_err(|e| TuiError::Other(e.to_string()))?
        } else {
            self.connect
                .store
                .clear_all()
                .map_err(|e| TuiError::Other(e.to_string()))?
        };
        if let Some(id) = profile_id {
            let _ = self.connect.preferences.clear_last_selection(Some(id));
        } else {
            let _ = self.connect.preferences.clear_last_selection(None);
        }
        self.refresh_connection_ui();
        let msg = if let Some(id) = profile_id {
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
            store: &self.connect.store,
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
            .find(|p| saved_selection.as_ref().is_some_and(|(id, _)| id == &p.id))
            .cloned();
        if let Some(profile) = chosen {
            // Refresh and inject provider credentials into the client only.
            let _ = svc.ensure_oauth_fresh(&profile.id);
            if let Ok(pairs) = svc.provider_env_for_profile(&profile.id) {
                self.session.apply_provider_env(&pairs);
            }
            self.connect.profile = Some(profile.id.clone());
            // The route decides the *transport*, and `transport_for_route(None)`
            // falls back to OpenAI-compat. Restoring the model without the route
            // therefore sends an Anthropic or Codex profile's requests over the
            // wrong wire, and every call fails until the user re-picks the model
            // — which is the only other path that sets it. The route follows the
            // profile, not the model, so it is restored either way below.
            self.session
                .set_active_route_id(route_id_for_profile(&profile.id));
            // Only switch the active model when it still looks like the forge default
            // (don't clobber an explicit --model / test runtime label).
            let cur = self.runtime.model_label.as_str();
            let looks_default =
                cur.is_empty() || cur == "openai/gpt-4.1-mini" || cur == "m" || cur == "mock";
            if looks_default {
                let saved_model = saved_selection
                    .as_ref()
                    .and_then(|(id, model)| (id == &profile.id).then_some(model.as_str()))
                    .filter(|model| {
                        let prefix = Self::model_prefix(model);
                        let pid = profile.id.as_str();
                        let provider_prefix = profile.model_provider_prefix.as_str();
                        prefix == pid
                            || prefix == provider_prefix
                            || (prefix == "openai" && pid == "openai_codex")
                            || (prefix == "openai-codex" && pid == "openai_codex")
                            || (prefix == "opencode-go" && pid == "opencode_go")
                            || (prefix == "opencode-zen" && pid == "opencode_zen")
                            || (prefix == "grok" && pid == "xai")
                    });
                if let Some(model) = saved_model.or_else(|| profile.default_model()) {
                    self.apply_selection(&ModelSelection {
                        route_id: self
                            .connect
                            .profile
                            .as_deref()
                            .map(route_id_for_profile)
                            .unwrap_or_default(),
                        provider: "native".into(),
                        model: model.to_string(),
                        profile_id: Some(profile.id.clone()),
                        effort: self.reasoning_effort.value.to_string(),
                    });
                }
            } else if self.session.active_model.is_empty() {
                self.session
                    .set_active_model(self.runtime.model_label.clone());
                self.sync_model_capabilities();
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
        self.session.set_reasoning_effort(value);
    }

    /// Re-read the active model's metadata from the models.dev registry
    /// cache: whether it accepts image input, and the context/output token
    /// limits compaction sizes itself from. Called after every model change.
    ///
    /// The single place `runtime.provider`, `runtime.model_label`,
    /// `session.active_model`, and `connect.profile` are set together (effort
    /// only when the selection carries a parseable one) — every model/route
    /// change must go through this rather than assigning the fields by hand,
    /// so the copies can't drift the way the picker's Enter-key bug once let
    /// a discarded selection produce a mismatched model id.
    pub(super) fn sync_model_capabilities(&mut self) {
        let cache = forge_connect::ModelCatalogCache::user_default();
        // Pre-feature catalog files can be "fresh" on TTL but have never
        // ingested `modalities.input`. Refresh once so Codex/API twins
        // (openai-codex/gpt-5.6-sol ↔ openai/gpt-5.6-sol) get a real flag.
        if !cfg!(test) && !cache.image_input_ready() {
            let _ = forge_connect::refresh_models_dev_registry(
                forge_connect::loaded_registry().profiles(),
                &cache,
            );
        }
        let supported = cache.model_accepts_image_input(&self.session.active_model);
        self.session.set_image_input_supported(supported);
        // Use one fixed context budget across providers. Keep the catalog's
        // output limit when available so reply headroom remains provider-aware.
        let output = cache
            .model_limits(&self.session.active_model)
            .and_then(|limits| (limits.output > 0).then_some(limits.output));
        self.session.set_context_window(500_000, output);
    }

    pub(super) fn apply_selection(&mut self, selection: &ModelSelection) {
        self.runtime.provider = if selection.provider.trim().is_empty() {
            "native".into()
        } else {
            selection.provider.clone()
        };
        self.runtime.model_label = selection.model.clone();
        self.session.set_active_model(&selection.model);
        self.session.set_active_route_id(&selection.route_id);
        self.sync_model_capabilities();
        self.connect.profile = selection.profile_id.clone();
        if let Ok(effort) = selection.effort.parse::<ReasoningEffort>() {
            self.reasoning_effort.value = effort;
        }
    }

    pub(super) fn persist_selection(&self) {
        if let Some(profile_id) = self.connect.profile.as_deref() {
            let _ = self
                .connect
                .preferences
                .set_last_selection(profile_id, &self.runtime.model_label);
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
                    route_id: route_id_for_profile(&profile_id),
                    provider: "native".into(),
                    model: model.clone(),
                    profile_id: Some(profile_id.clone()),
                    effort,
                });
                self.apply_connect_credentials(&profile_id);
                self.feedback = FeedbackModel::default();
                self.status_state.message.clear();
                self.notice_state.items.clear();
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
                store: &self.connect.store,
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
        let providers = build_provider_rows(
            &self.connect.registry,
            &connected,
            self.connect.profile.as_deref(),
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
        self.overlay = Some(self.build_connect_model_overlay(ConnectModelColumn::Providers, false));
        self.status_state.message = "Choose a provider".into();
        self.notice_state.items.clear();
        self.start_catalog_refresh();
    }

    /// Open the persistent footer control's compact picker, focused on
    /// `focus`, without disturbing the conversation underneath.
    pub(super) fn open_connect_picker_compact(&mut self, focus: ConnectModelColumn) {
        self.overlay = Some(self.build_connect_model_overlay(focus, true));
        self.start_catalog_refresh();
    }

    fn open_api_key_prompt(&mut self, profile_id: &str, error: Option<String>) {
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
        self.notice_state.items.clear();
    }

    /// After a successful connect, either land directly in a usable steady
    /// state (this was the first-ever connected route: auto-select a
    /// default model + effort so onboarding doesn't force a manual picker,
    /// matching "select a valid catalog/default model... then transition to
    /// the normal state") or open the model picker as today (a routine
    /// additional/re-connect, where browsing models is the deliberate point
    /// of the action).
    pub(super) fn finish_connect_flow(&mut self, profile_id: &str) {
        if self.connected_profile_count() == 1 {
            self.apply_default_model_for_profile(profile_id, "connected");
        } else {
            self.open_model_picker_after_connect(profile_id);
        }
    }

    fn connected_profile_count(&self) -> usize {
        let svc = ConnectService {
            registry: &self.connect.registry,
            store: &self.connect.store,
            preferences: &self.connect.preferences,
            active_profile_id: self.connect.profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        svc.connected_profiles().map(|v| v.len()).unwrap_or(0)
    }

    /// Activate `profile_id` (assumed already connected) as the current
    /// route and auto-select a model for it, landing in normal steady state
    /// without a forced next step. Reused by both the post-connect
    /// completion path (`finish_connect_flow`, `verb: "connected"`) and the
    /// Providers picker's standalone route-switch action
    /// (`OverlayAction::SwitchToRoute`, `verb: "active"` — the route was
    /// already connected in a past session, this is just a switch).
    ///
    /// Uses the explicit first-run fallback projection, which can fall back
    /// to `profile.default_models` even with an empty cache (see
    /// `models_for_picker` in forge-connect), so this works offline / on a
    /// cold cache. Falls back to the picker if the
    /// profile somehow has no usable model at all, rather than stranding
    /// the user with an empty `active_model`.
    pub(super) fn apply_default_model_for_profile(&mut self, profile_id: &str, verb: &str) {
        self.connect.profile = Some(profile_id.to_string());
        let model = self
            .model_picker_items_with_defaults(false)
            .into_iter()
            .find(|item| item.profile_id.as_deref() == Some(profile_id))
            .map(|item| item.model);
        let Some(model) = model else {
            self.open_model_picker_after_connect(profile_id);
            return;
        };
        let effort = ReasoningEffort::default_for_model(&model);
        self.apply_selection(&ModelSelection {
            route_id: route_id_for_profile(profile_id),
            provider: "native".into(),
            model: model.clone(),
            profile_id: Some(profile_id.to_string()),
            effort: effort.to_string(),
        });
        self.persist_selection();
        self.record_deliberate_selection();
        self.overlay = None;
        self.onboarding_connect = false;
        self.notice_state.items.clear();
        let title = self
            .connect
            .registry
            .get(profile_id)
            .map(|p| p.title.as_str())
            .unwrap_or(profile_id);
        self.set_feedback(
            FeedbackSeverity::Ok,
            format!("{title} {verb} · {model} ready"),
        );
        self.start_catalog_refresh();
    }

    pub(super) fn open_model_picker_after_connect(&mut self, profile_id: &str) {
        let overlay = self.build_connect_model_overlay_scoped(
            ConnectModelColumn::Models,
            false,
            Some(profile_id),
        );
        self.overlay = Some(overlay);
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
        self.notice_state.items.clear();
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
                store: &self.connect.store,
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
        let providers = build_provider_rows(
            &self.connect.registry,
            &connected,
            self.connect.profile.as_deref(),
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
                // Also fill notices with list for accessibility
                let mut model = Some(self.runtime.model_label.clone());
                if let Ok(msg) = handle_connect_action(
                    ConnectAction::List,
                    &self.connect.registry,
                    &self.connect.store,
                    &self.connect.preferences,
                    &mut self.connect.profile,
                    &mut model,
                ) {
                    self.push_notice(msg.lines().map(|s| s.to_string()).collect());
                }
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
            &self.connect.store,
            &self.connect.preferences,
            &mut self.connect.profile,
            &mut model,
        ) {
            Ok(msg) => {
                if let Some(m) = model {
                    self.runtime.model_label = m.clone();
                    self.runtime.provider = "native".into();
                    self.connect.auth_suspended = false;
                    self.session.set_active_model(m);
                    self.sync_model_capabilities();
                }
                if let Some(pid) = self.connect.profile.clone() {
                    self.apply_connect_credentials(&pid);
                }
                let lines: Vec<String> = msg.lines().map(|s| s.to_string()).collect();
                self.status_state.message = lines.first().cloned().unwrap_or_default();
                self.notice_state.items.clear();
                self.notice_state.until = None;
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
                let error = e.to_string();
                if let Some(profile_id) =
                    connect_target.filter(|id| needs_tui_api_key_prompt(&self.connect.registry, id))
                {
                    self.open_api_key_prompt(&profile_id, Some(error));
                } else {
                    self.status_state.message = error.clone();
                    self.push_notice_with_severity(vec![error], FeedbackSeverity::Error);
                }
            }
        }
    }

    pub(super) fn begin_oauth_flow(&mut self, profile_id: &str) {
        let mut svc = ConnectService {
            registry: &self.connect.registry,
            store: &self.connect.store,
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
                self.push_notice_with_severity(vec![e.to_string()], FeedbackSeverity::Error);
                self.report_error(&e.to_string());
            }
        }
    }

    /// After a successful connect: update model, inject credentials, clear OAuth UI.
    fn on_connect_success(&mut self, out: &forge_connect::ConnectOutcome) {
        self.connect.profile = Some(out.profile_id.clone());
        self.runtime.model_label = out.model.clone();
        self.runtime.provider = "native".into();
        self.connect.auth_suspended = false;
        self.session.set_active_model(out.model.clone());
        self.sync_model_capabilities();
        self.apply_connect_credentials(&out.profile_id);
        self.connect.oauth_pending = None;
        self.connect.oauth_last_poll = None;
        self.refresh_connection_ui();
        self.finish_connect_flow(&out.profile_id);
    }

    /// Export stored OAuth / API key material into the native model client.
    pub(super) fn apply_connect_credentials(&mut self, profile_id: &str) {
        let svc = ConnectService {
            registry: &self.connect.registry,
            store: &self.connect.store,
            preferences: &self.connect.preferences,
            active_profile_id: self.connect.profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        match svc.provider_env_for_profile(profile_id) {
            Ok(pairs) if !pairs.is_empty() => {
                // Given to the client only. `NativeModelClient::injected_or_env`
                // reads the injected map ahead of the process environment, so
                // exporting these as well changed nothing for the client — it
                // only made every child process inherit them.
                self.session.apply_provider_env(&pairs);
            }
            Ok(_) => {}
            Err(_e) => {
                // Non-fatal: operator can still set XAI_API_KEY in the shell.
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
        let lines: Vec<String> = instructions.lines().map(|s| s.to_string()).collect();
        self.status_state.message = lines
            .first()
            .cloned()
            .unwrap_or_else(|| format!("OAuth for {}", pending.profile_id));
        self.push_notice(lines);
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
            store: &self.connect.store,
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
            &self.connect.store,
            &self.connect.preferences,
            &mut self.connect.profile,
            &mut model,
        ) {
            Ok(msg) => {
                if let Some(m) = model {
                    self.runtime.model_label = m.clone();
                    self.runtime.provider = "native".into();
                    self.connect.auth_suspended = false;
                    self.session.set_active_model(m);
                    self.sync_model_capabilities();
                }
                if let Some(pid) = self.connect.profile.clone() {
                    self.apply_connect_credentials(&pid);
                }
                self.status_state.message = msg.lines().next().unwrap_or_default().to_string();
                self.refresh_connection_ui();
                self.finish_connect_flow(profile_id);
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
                    store: &self.connect.store,
                    preferences: &self.connect.preferences,
                    active_profile_id: None,
                    active_model: None,
                };
                svc.connected_profiles().ok().and_then(|connected| {
                    connected
                        .iter()
                        .find(|p| {
                            p.model_provider_prefix == prefix
                                || p.id == prefix
                                || (prefix == "opencode-go" && p.id == "opencode_go")
                                || (prefix == "opencode-zen" && p.id == "opencode_zen")
                        })
                        .map(|p| p.id.clone())
                })
            }
        };
        // Effort is untouched here — the caller resolves it separately (see
        // `resolve_effort_for_model`), so carry the current value through
        // unchanged rather than resetting it.
        self.apply_selection(&ModelSelection {
            route_id: profile_id.map(route_id_for_profile).unwrap_or_default(),
            provider: provider.to_string(),
            model: model.to_string(),
            profile_id: resolved_profile_id,
            effort: self.reasoning_effort.value.to_string(),
        });
        if let Some(profile_id) = self.connect.profile.clone() {
            self.apply_connect_credentials(&profile_id);
        }
        self.record_deliberate_selection();
        self.feedback = FeedbackModel::default();
        self.status_state.message.clear();
        self.notice_state.items.clear();
        self.push_activity(
            ActivityKind::System,
            FeedbackSeverity::Ok,
            format!("model {}", self.runtime.model_label),
        );
    }

    pub(super) fn model_prefix(model: &str) -> &str {
        model.split('/').next().unwrap_or("").trim()
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
            store: &self.connect.store,
            preferences: &self.connect.preferences,
            active_profile_id: self.connect.profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        let connected = svc.connected_profiles().unwrap_or_default();
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
        if self.connected_profile_count() == 0 {
            return Vec::new();
        }
        let profiles = self.picker_profiles();
        let cache = ModelCatalogCache::user_default();
        let entries =
            runnable_models_for_picker(&profiles, &self.connect.store, &cache, refresh_stale);
        let mut items = models_from_catalog(&entries);
        for item in &mut items {
            if let Some(profile_id) = item.profile_id.as_deref() {
                item.route_label = self.format_route_label(profile_id);
            }
        }
        items
    }

    /// Build rows including public/default fallback tiers. Used only when a
    /// newly connected route needs its first safe model.
    fn model_picker_items_with_defaults(
        &self,
        refresh_stale: bool,
    ) -> Vec<crate::overlays::ModelItem> {
        let profiles = self.picker_profiles();
        let cache = ModelCatalogCache::user_default();
        let entries = models_for_picker(&profiles, &self.connect.store, &cache, refresh_stale);
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

    /// Warm the catalog cache once, the first event-loop tick a connected
    /// profile is active, so the footer control and picker have live data
    /// without requiring `/connect`/`/model` first. Deliberately an
    /// event-loop tick, not `draw()` or app construction: `draw()` must stay
    /// a side-effect-free projection of state, and firing at construction
    /// time would race a caller's very first frame (a real cost in tests and
    /// tools that construct a `TuiApp` and render once, since the spawned
    /// thread's network I/O has no bound on when it finishes relative to
    /// that first render).
    pub(super) fn warm_catalog_once_connected(&mut self) {
        if self.catalog_fetch.warmed {
            return;
        }
        if !self.is_provider_connected() || self.is_mock_provider() {
            return;
        }
        self.catalog_fetch.warmed = true;
        self.start_catalog_refresh();
    }

    /// Kick off a background catalog refresh if one isn't already in flight.
    /// Never blocks: the network I/O runs on a spawned thread (matching
    /// `poll_repo_header`'s shape) and writes through to the on-disk
    /// `ModelCatalogCache` as a side effect; `poll_catalog_refresh` (called
    /// once per event-loop tick, never from `draw()`) picks up completion
    /// and refreshes any open picker's rows from the now-warm cache.
    /// Account catalogs are always re-fetched here; the disk cache is only
    /// the instant-open / offline fallback.
    pub(super) fn start_catalog_refresh(&mut self) {
        if self.catalog_fetch.refresh_rx.is_some() {
            return;
        }
        let profiles = self.picker_profiles();
        let store_path = self.connect.store.path().to_path_buf();
        let cache_path = ModelCatalogCache::user_default().path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let store = CredentialStore::new(store_path);
            let cache = ModelCatalogCache::new(cache_path);
            models_for_picker(&profiles, &store, &cache, true);
            let _ = tx.send(Ok(()));
        });
        self.catalog_fetch.refresh_rx = Some(rx);
        if let Some(overlay) = &mut self.overlay {
            overlay.set_catalog_loading(true);
        }
    }

    /// Non-blocking poll for a finished background catalog refresh. Safe to
    /// call every event-loop tick; no-ops while nothing is in flight.
    pub(super) fn poll_catalog_refresh(&mut self) {
        let Some(rx) = self.catalog_fetch.refresh_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(_) => {
                self.refresh_open_picker_items();
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.catalog_fetch.refresh_rx = Some(rx);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // Worker panicked or dropped without sending — drop the
                // handle so the next trigger can retry, and stop showing
                // "Loading models…" for a refresh that's never coming.
                if let Some(overlay) = &mut self.overlay {
                    overlay.set_catalog_loading(false);
                }
            }
        }
    }

    /// Re-read the catalog from the now-warm disk cache and refresh an open
    /// `ConnectModel` overlay's rows in place, if one is open.
    fn refresh_open_picker_items(&mut self) {
        if !matches!(self.overlay, Some(Overlay::ConnectModel { .. })) {
            return;
        }
        let items = self.model_picker_items(false);
        if let Some(overlay) = &mut self.overlay {
            overlay.refresh_model_items(items);
            overlay.set_catalog_loading(false);
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

fn route_id_for_profile(profile_id: &str) -> String {
    forge_connect::loaded_registry()
        .get(profile_id)
        .map(|spec| spec.route_id.clone())
        .unwrap_or_else(|| profile_id.to_string())
}
