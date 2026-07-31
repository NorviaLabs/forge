//! Provider connection, credentials and model selection for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Covers the `/connect` flow end to end: the
//! picker, API-key entry, the OAuth device dance and its polling, credential
//! persistence, and the model and reasoning-effort selection that follows a
//! successful connection. Methods are moved verbatim.

use super::*;

impl TuiApp {
    /// Mock provider is always "connected" (offline tests / CI).
    fn is_mock_provider(&self) -> bool {
        self.runtime.provider.eq_ignore_ascii_case("mock")
            || self.runtime.model_label.eq_ignore_ascii_case("mock")
    }

    /// Live credentials still exist for a connect profile id.
    fn credentials_live_for(&self, profile_id: &str) -> bool {
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        svc.connected_profiles()
            .ok()
            .map(|ps| ps.iter().any(|p| p.id == profile_id))
            .unwrap_or(false)
    }

    /// True when chat may call an LLM (mock, or a live `/connect` profile).
    pub fn is_provider_connected(&self) -> bool {
        if self.is_mock_provider() {
            return true;
        }
        match self.connect_profile.as_deref() {
            Some(id) => self.credentials_live_for(id),
            None => false,
        }
    }

    /// Drop stale `connect_profile` if credentials were cleared out-of-band.
    fn sync_provider_connection(&mut self) {
        if self.auth_suspended {
            return;
        }
        if self.is_mock_provider() {
            return;
        }
        if let Some(id) = self.connect_profile.clone() {
            if !self.credentials_live_for(&id) {
                self.connect_profile = None;
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
        let connected = self.is_provider_connected();
        self.input.not_connected = !connected;
        if connected {
            if self.input.hint.contains("Not connected") || self.input.hint.contains("/connect") {
                self.input.hint = String::new();
            }
            // Drop the sticky not-connected banner once signed in.
            self.ui_banners.retain(|b| {
                !matches!(
                    b,
                    ChatItem::Banner {
                        kind: BannerKind::Warn,
                        text
                    } if text.contains("Not connected")
                )
            });
        } else {
            self.input.hint = "Not connected · run /connect before chatting".into();
            let has_banner = self.ui_banners.iter().any(|b| {
                matches!(
                    b,
                    ChatItem::Banner {
                        kind: BannerKind::Warn,
                        text
                    } if text.contains("Not connected")
                )
            });
            if !has_banner {
                self.ui_banners.push(ChatItem::Banner {
                    text: "Not connected to an LLM provider. Run /connect (xAI Grok or OpenCode Go) before sending a message.".into(),
                    kind: BannerKind::Warn,
                });
            }
        }
    }

    pub(super) fn disconnect_auth(&mut self, profile_id: Option<&str>) -> Result<String, TuiError> {
        let mut env_keys = Vec::new();
        {
            let svc = ConnectService {
                registry: &self.connect_registry,
                store: &self.connect_store,
                active_profile_id: self.connect_profile.clone(),
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
        self.oauth_pending = None;
        self.oauth_last_poll = None;
        self.pending_prompt = None;
        self.pending_hitl_decision = None;
        self.pending_context_reset = false;
        self.message_queue = MessageQueue::new();
        self.queue_selected = None;
        self.stream_preview.clear();
        self.stream_thinking.clear();
        self.turn_started = None;
        self.thinking_started = None;
        self.thought_secs = None;
        self.cancel_requested = false;
        self.busy = false;
        self.busy_phase = BusyPhase::Idle;
        self.tool_expanded = false;
        self.chat_follow = true;
        self.chat_scroll = 0;
        self.connect_profile = None;
        self.runtime.provider.clear();
        self.runtime.model_label.clear();
        self.session.set_active_model(String::new());
        self.feedback = FeedbackModel::default();
        self.status_message = "disconnected".into();
        self.notices.clear();
        self.ui_banners.retain(|b| {
            !matches!(
                b,
                ChatItem::Banner {
                    kind: BannerKind::Warn,
                    text
                } if text.contains("Not connected")
            )
        });
        self.auth_suspended = true;

        let cleared = if let Some(id) = profile_id {
            self.connect_store
                .clear(id)
                .map_err(|e| TuiError::Other(e.to_string()))?
        } else {
            self.connect_store
                .clear_all()
                .map_err(|e| TuiError::Other(e.to_string()))?
        };
        if let Some(id) = profile_id {
            let _ = self.connect_store.clear_last_selection(Some(id));
        } else {
            let _ = self.connect_store.clear_last_selection(None);
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
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: None,
            active_model: None,
        };
        let mut connected = svc.connected_profiles().unwrap_or_default();
        connected.sort_by(|a, b| a.id.cmp(&b.id));
        let saved_selection = self.connect_store.last_selection().ok().flatten();
        if let Some(effort) = self
            .connect_store
            .last_effort()
            .ok()
            .flatten()
            .and_then(|effort| effort.parse().ok())
        {
            self.reasoning_effort = effort;
        }
        // Restore the last usable provider; otherwise fall back to a deterministic
        // connected profile instead of silently preferring one backend family.
        let chosen = connected
            .iter()
            .find(|p| saved_selection.as_ref().is_some_and(|(id, _)| id == &p.id))
            .or_else(|| connected.first())
            .cloned();
        if let Some(profile) = chosen {
            // Refresh and inject provider credentials into the client only.
            let _ = svc.ensure_oauth_fresh(&profile.id);
            if let Ok(pairs) = svc.provider_env_for_profile(&profile.id) {
                self.session.apply_provider_env(&pairs);
            }
            self.connect_profile = Some(profile.id.clone());
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
                    self.runtime.model_label = model.to_string();
                    self.runtime.provider = "native".into();
                    self.session.set_active_model(model);
                }
            } else if self.session.active_model.is_empty() {
                self.session
                    .set_active_model(self.runtime.model_label.clone());
            }
            self.status_message = format!("restored {} · {}", profile.id, self.runtime.model_label);
        }
        self
    }

    pub(super) fn open_effort_picker_for_model(&mut self, model: &str) {
        let options = ReasoningEffort::options_for_model(model);
        let default = ReasoningEffort::default_for_model(model);
        if options.len() <= 1 {
            // Nothing useful to choose; keep current if still valid, else provider default.
            if !options.contains(&self.reasoning_effort) {
                self.reasoning_effort = default;
                self.persist_selection();
            }
            self.overlay = None;
            return;
        }
        if !options.contains(&self.reasoning_effort) {
            self.reasoning_effort = default;
        }
        self.overlay = Some(Overlay::effort_open(model, self.reasoning_effort));
        self.set_feedback(FeedbackSeverity::Info, "choose reasoning effort");
    }

    pub(super) fn persist_selection(&self) {
        if let Some(profile_id) = self.connect_profile.as_deref() {
            let _ = self
                .connect_store
                .set_last_selection(profile_id, &self.runtime.model_label);
        }
        let _ = self
            .connect_store
            .set_last_effort(&self.reasoning_effort.to_string());
    }

    pub(super) fn open_connect_picker(&mut self) {
        let connected: HashSet<String> = {
            let svc = ConnectService {
                registry: &self.connect_registry,
                store: &self.connect_store,
                active_profile_id: self.connect_profile.clone(),
                active_model: Some(self.runtime.model_label.clone()),
            };
            svc.connected_profiles()
                .unwrap_or_default()
                .into_iter()
                .map(|p| p.id)
                .collect()
        };
        let items: Vec<ConnectProfileItem> = self
            .connect_registry
            .profiles()
            .iter()
            .map(|p| ConnectProfileItem {
                id: p.id.clone(),
                title: p.title.clone(),
                auth_mode: p.auth_mode.label().into(),
                auth_url: p.auth_url.clone(),
                connected: connected.contains(&p.id),
            })
            .collect();
        self.overlay = Some(Overlay::connect_picker(items));
        self.status_message = "Choose a provider".into();
        self.notices.clear();
    }

    fn open_api_key_prompt(&mut self, profile_id: &str, error: Option<String>) {
        let p = self.connect_registry.get(profile_id);
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
        self.status_message = format!("Connect {profile_id}");
        self.notices.clear();
    }

    pub(super) fn open_model_picker_after_connect(&mut self, profile_id: &str) {
        let items = self.model_picker_items(true);
        let mut overlay = Overlay::model_open_with(items);
        overlay.focus_model(&self.runtime.model_label);
        self.overlay = Some(overlay);
        let title = self
            .connect_registry
            .get(profile_id)
            .map(|p| p.title.as_str())
            .unwrap_or(profile_id);
        self.set_feedback(
            FeedbackSeverity::Ok,
            format!("{title} connected · choose a model"),
        );
        self.notices.clear();
    }

    pub(super) fn handle_connect(&mut self, action: ConnectAction) {
        // /connect or /connect list → interactive profile picker (usable UX)
        match &action {
            ConnectAction::Open | ConnectAction::List => {
                self.open_connect_picker();
                // Also fill notices with list for accessibility
                let mut model = Some(self.runtime.model_label.clone());
                if let Ok(msg) = handle_connect_action(
                    ConnectAction::List,
                    &self.connect_registry,
                    &self.connect_store,
                    &mut self.connect_profile,
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
                if needs_tui_api_key_prompt(&self.connect_registry, profile_id) {
                    // Existing file/env credentials should reconnect without
                    // asking the user to paste the same secret again.
                    if !self.credentials_live_for(profile_id) {
                        self.open_api_key_prompt(profile_id, None);
                        return;
                    }
                }
                if needs_tui_oauth(&self.connect_registry, profile_id) {
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
            &self.connect_registry,
            &self.connect_store,
            &mut self.connect_profile,
            &mut model,
        ) {
            Ok(msg) => {
                if let Some(m) = model {
                    self.runtime.model_label = m.clone();
                    self.runtime.provider = "native".into();
                    self.auth_suspended = false;
                    self.session.set_active_model(m);
                }
                if let Some(pid) = self.connect_profile.clone() {
                    self.apply_connect_credentials(&pid);
                }
                let lines: Vec<String> = msg.lines().map(|s| s.to_string()).collect();
                self.status_message = lines.first().cloned().unwrap_or_default();
                self.notices.clear();
                self.notices_until = None;
                self.notices_until = None;
                self.notices_until = None;
                self.push_activity(
                    ActivityKind::Connect,
                    FeedbackSeverity::Ok,
                    self.status_message.clone(),
                );
                if let Some(line) = lines.first() {
                    self.push_toast(line.clone());
                }
                self.refresh_connection_ui();
                if let Some(profile_id) = connect_target {
                    self.open_model_picker_after_connect(&profile_id);
                }
            }
            Err(ConnectError::OauthDevicePending(pending)) => {
                // Payload is boxed so `ConnectError` stays small in every `Result`.
                self.show_oauth_pending(*pending);
            }
            Err(e) => {
                let error = e.to_string();
                if let Some(profile_id) =
                    connect_target.filter(|id| needs_tui_api_key_prompt(&self.connect_registry, id))
                {
                    self.open_api_key_prompt(&profile_id, Some(error));
                } else {
                    self.status_message = error.clone();
                    self.push_notice(vec![error]);
                }
            }
        }
    }

    pub(super) fn begin_oauth_flow(&mut self, profile_id: &str) {
        let mut svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        match svc.connect_start_oauth(profile_id) {
            Ok(Ok(out)) => {
                self.on_connect_success(&out);
            }
            Ok(Err(pending)) => self.show_oauth_pending(pending),
            Err(e) => {
                self.status_message = e.to_string();
                self.push_notice(vec![e.to_string()]);
                self.report_error(&e.to_string());
            }
        }
    }

    /// After a successful connect: update model, inject credentials, clear OAuth UI.
    fn on_connect_success(&mut self, out: &forge_connect::ConnectOutcome) {
        self.connect_profile = Some(out.profile_id.clone());
        self.runtime.model_label = out.model.clone();
        self.runtime.provider = "native".into();
        self.auth_suspended = false;
        self.session.set_active_model(out.model.clone());
        self.apply_connect_credentials(&out.profile_id);
        self.oauth_pending = None;
        self.oauth_last_poll = None;
        self.refresh_connection_ui();
        self.open_model_picker_after_connect(&out.profile_id);
    }

    /// Export stored OAuth / API key material into the native model client.
    pub(super) fn apply_connect_credentials(&mut self, profile_id: &str) {
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
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

    fn show_oauth_pending(&mut self, pending: OauthPending) {
        let title = self
            .connect_registry
            .get(&pending.profile_id)
            .map(|p| p.title.clone())
            .unwrap_or_else(|| pending.profile_id.clone());
        let instructions = pending.operator_instructions();
        let lines: Vec<String> = instructions.lines().map(|s| s.to_string()).collect();
        self.status_message = lines
            .first()
            .cloned()
            .unwrap_or_else(|| format!("OAuth for {}", pending.profile_id));
        self.push_notice(lines);
        self.overlay = Some(Overlay::connect_oauth(
            pending.profile_id.clone(),
            title,
            instructions,
        ));
        self.oauth_pending = Some(pending);
        self.oauth_last_poll = None;
    }

    /// Poll device-code OAuth once (called from the TUI tick loop).
    pub fn poll_oauth_tick(&mut self) {
        let Some(pending) = self.oauth_pending.clone() else {
            return;
        };
        let interval = Duration::from_secs(pending.interval_secs.max(1));
        if let Some(last) = self.oauth_last_poll {
            if last.elapsed() < interval {
                return;
            }
        }
        self.oauth_last_poll = Some(std::time::Instant::now());
        let mut svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
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
                self.oauth_pending = None;
                self.oauth_last_poll = None;
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
            &self.connect_registry,
            &self.connect_store,
            &mut self.connect_profile,
            &mut model,
        ) {
            Ok(msg) => {
                if let Some(m) = model {
                    self.runtime.model_label = m.clone();
                    self.runtime.provider = "native".into();
                    self.auth_suspended = false;
                    self.session.set_active_model(m);
                }
                if let Some(pid) = self.connect_profile.clone() {
                    self.apply_connect_credentials(&pid);
                }
                self.status_message = msg.lines().next().unwrap_or_default().to_string();
                self.refresh_connection_ui();
                self.open_model_picker_after_connect(profile_id);
            }
            Err(e) => {
                let err = e.to_string();
                self.overlay = saved_overlay;
                if let Some(Overlay::ConnectApiKey { error, .. }) = &mut self.overlay {
                    *error = Some(err.clone());
                }
                self.status_message = err;
                self.push_activity(
                    ActivityKind::Connect,
                    FeedbackSeverity::Error,
                    format!("connect {profile_id} failed"),
                );
            }
        }
    }

    /// Apply a provider/model id to this session (no restart required).
    pub(super) fn apply_model_selection(&mut self, provider: &str, model: &str) {
        let model = model.trim();
        if model.is_empty() {
            return;
        }
        self.runtime.provider = if provider.trim().is_empty() {
            "native".into()
        } else {
            provider.to_string()
        };
        self.auth_suspended = false;
        self.runtime.model_label = model.to_string();
        self.session.set_active_model(model);
        // Match the selected model to its connected profile even when a
        // different provider was active before opening the picker.
        let prefix = model.split('/').next().unwrap_or("");
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: None,
            active_model: None,
        };
        if let Ok(connected) = svc.connected_profiles() {
            if let Some(profile) = connected.iter().find(|p| {
                p.model_provider_prefix == prefix
                    || p.id == prefix
                    || (prefix == "opencode-go" && p.id == "opencode_go")
                    || (prefix == "opencode-zen" && p.id == "opencode_zen")
            }) {
                self.connect_profile = Some(profile.id.clone());
            }
        }
        if let Some(profile_id) = self.connect_profile.clone() {
            self.apply_connect_credentials(&profile_id);
        }
        self.persist_selection();
        self.feedback = FeedbackModel::default();
        self.status_message.clear();
        self.notices.clear();
        self.push_activity(
            ActivityKind::System,
            FeedbackSeverity::Ok,
            format!("model {}", self.runtime.model_label),
        );
    }

    pub(super) fn model_prefix(model: &str) -> &str {
        model.split('/').next().unwrap_or("").trim()
    }

    pub(super) fn connected_profile_for_model_prefix(&self, prefix: &str) -> Option<String> {
        let prefix = prefix.trim();
        if prefix.is_empty() {
            return None;
        }
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        let connected = svc.connected_profiles().ok()?;
        connected.iter().find_map(|profile| {
            let pid = profile.id.as_str();
            let provider_prefix = profile.model_provider_prefix.as_str();
            let matches = prefix == pid
                || prefix == provider_prefix
                || (prefix == "openai" && pid == "openai_codex")
                || (prefix == "openai-codex" && pid == "openai_codex")
                || (prefix == "opencode-go" && pid == "opencode_go")
                || (prefix == "opencode-zen" && pid == "opencode_zen")
                || (prefix == "grok" && pid == "xai");
            if matches {
                Some(profile.id.clone())
            } else {
                None
            }
        })
    }

    /// Build `/model` picker rows from connected-profile catalogs (cache + optional refresh).
    pub(super) fn model_picker_items(
        &self,
        refresh_stale: bool,
    ) -> Vec<crate::overlays::ModelItem> {
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        let connected = svc.connected_profiles().unwrap_or_default();
        let cache = ModelCatalogCache::user_default();
        let profiles: Vec<_> = if connected.is_empty() {
            // Show all built-in defaults when nothing connected
            self.connect_registry.profiles().to_vec()
        } else {
            connected
        };
        let entries = models_for_picker(&profiles, &self.connect_store, &cache, refresh_stale);
        models_from_catalog(&entries)
    }

    #[allow(dead_code)]
    fn active_model_cost(&mut self) -> Option<forge_connect::CatalogCost> {
        if let Some((model, cost)) = &self.model_cost_cache {
            if model == &self.runtime.model_label {
                return *cost;
            }
        }
        let cost = ModelCatalogCache::user_default().get_registry_cost(&self.runtime.model_label);
        self.model_cost_cache = Some((self.runtime.model_label.clone(), cost));
        cost
    }
}
