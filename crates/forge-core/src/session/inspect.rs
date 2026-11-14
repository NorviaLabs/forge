//! Read-only reporting over a session, plus the small setters that
//! configure the next model call.
//!
//! Split out of `lib.rs`; methods are moved verbatim.

use crate::*;

impl AgentSession {
    /// Snapshot the request-routing state as one value so callers cannot
    /// accidentally persist or display only part of a selection.
    pub fn model_selection(&self) -> crate::ModelExecutionSelection {
        crate::ModelExecutionSelection {
            model: self.active_model.clone(),
            profile_id: self.active_profile_id.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            thinking_enabled: self.thinking_enabled,
        }
    }

    /// Apply all execution-facing model state together.
    ///
    /// An explicit selection replaces any turn-scoped model tier: the user's
    /// choice is what the session returns to, not the model the tier
    /// replaced.
    pub fn set_model_selection(&mut self, selection: crate::ModelExecutionSelection) {
        // Back on the user's own model and its capabilities first, so nothing
        // of the tier's model outlives it.
        self.restore_turn_tier();
        self.set_model_selection_keeping_tier(selection);
    }

    /// [`Self::set_model_selection`] for the tier machinery itself.
    pub(crate) fn set_model_selection_keeping_tier(
        &mut self,
        selection: crate::ModelExecutionSelection,
    ) {
        self.active_model = selection.model;
        self.active_profile_id = selection.profile_id;
        self.reasoning_effort = selection.reasoning_effort;
        self.thinking_enabled = selection.thinking_enabled;
        self.tool_ctx.active_model = self.active_model.clone();
    }

    pub fn journal_dir(&self) -> &std::path::Path {
        self.journal.directory()
    }

    /// Number of project/global skills available to the current session.
    /// This is intentionally a count only: skill contents remain model context.
    pub fn loaded_skills_count(&self) -> usize {
        self.context.skill_count()
    }

    /// Names of project/global skills available to the current session.
    pub fn loaded_skill_names(&self) -> Vec<String> {
        self.context
            .load_skills()
            .iter()
            .map(|skill| skill.name.clone())
            .collect()
    }

    pub fn loaded_skills(&self) -> Vec<forge_context::SkillManifest> {
        (*self.context.load_skills()).clone()
    }

    pub fn list_tools(&self) -> Vec<String> {
        let desc = self.tools.list_descriptors();
        let names = if self.enable_gov {
            self.governance
                .filter_tools((*desc).clone())
                .into_iter()
                .map(|t| t.name)
                .collect()
        } else {
            self.tools.names()
        };
        names
            .into_iter()
            .filter(|name| self.tool_within_limit(name))
            .collect()
    }

    /// Every registered tool with its side-effect class, before governance
    /// or a tool limit. What a custom agent's `tools` list can name.
    pub fn tool_classes(&self) -> Vec<(String, SideEffectClass)> {
        self.tools
            .name_classes()
            .map(|(name, class)| (name.to_string(), class))
            .collect()
    }

    /// How many tools this session would expose to the model.
    ///
    /// Equivalent to `list_tools().len()`, without building the list.
    /// Session snapshots ask for this on every refresh, where materialising
    /// every descriptor — name, description and a cloned input schema apiece
    /// — would be wasted allocation.
    pub fn tool_count(&self) -> usize {
        if self.enable_gov {
            self.tools
                .name_classes()
                .filter(|(name, class)| {
                    self.governance
                        .acl
                        .is_allowed(&self.governance.principal, name, *class)
                        && self.tool_within_limit(name)
                })
                .count()
        } else if self.tool_limit.is_some() {
            self.tools
                .name_classes()
                .filter(|(name, _)| self.tool_within_limit(name))
                .count()
        } else {
            self.tools.len()
        }
    }

    pub fn context_usage_ratio(&self) -> f64 {
        self.context_tokens_estimate() as f64 / self.context.config.capacity_tokens.max(1) as f64
    }

    /// Estimated tokens the tool schemas actually sent to the model would
    /// cost — exactly `tools_for_model`'s output, after governance filtering
    /// and tool-search deferral, so this reflects the deferral's savings
    /// rather than the pre-deferral set. This lives outside `self.messages`,
    /// so nothing else in this report accounts for it, even though it's
    /// resent on every single completion.
    fn tool_schema_tokens_estimate(&self) -> usize {
        self.tools_for_model().iter().map(descriptor_tokens).sum()
    }

    /// Estimated in-context tokens for `self.messages`, memoized across frames.
    ///
    /// Agent turns normally append immutable messages. For that hot path, the
    /// cache recognizes the same transcript allocation and estimates only the
    /// appended suffix. Replacements, tail edits, and copy-on-write splits fall
    /// back to a complete recount to preserve accuracy.
    fn context_tokens_estimate(&self) -> usize {
        let fingerprint = CtxTokensFingerprint::of(&self.messages);
        let mut cache = self
            .ctx_tokens_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = cache.as_ref() {
            if cached.fingerprint == fingerprint {
                return cached.total;
            }
            let is_same_transcript = cached.storage_id == self.messages.storage_id();
            if is_same_transcript && cached.fingerprint.messages < fingerprint.messages {
                let appended = messages_tokens(&self.messages[cached.fingerprint.messages..]);
                let total = cached.total.saturating_add(appended);
                *cache = Some(CtxTokensCache {
                    storage_id: self.messages.storage_id(),
                    fingerprint,
                    total,
                });
                return total;
            }
        }
        let total = messages_tokens(&self.messages);
        *cache = Some(CtxTokensCache {
            storage_id: self.messages.storage_id(),
            fingerprint,
            total,
        });
        total
    }

    /// Full token-usage report for status UIs (API totals + in-context estimates). No $.
    pub fn token_usage_report(&self) -> TokenUsageReport {
        let mut system_tokens_est = 0usize;
        let mut user_tokens_est = 0usize;
        let mut assistant_tokens_est = 0usize;
        let mut tool_tokens_est = 0usize;
        let mut thinking_in_context_est = 0usize;
        let mut tool_message_count = 0usize;
        for m in &self.messages {
            let n = estimate_tokens(&m.content);
            match m.role {
                MessageRole::System => system_tokens_est = system_tokens_est.saturating_add(n),
                MessageRole::User => user_tokens_est = user_tokens_est.saturating_add(n),
                MessageRole::Assistant => {
                    assistant_tokens_est = assistant_tokens_est.saturating_add(n);
                    if let Some(ref th) = m.thinking {
                        thinking_in_context_est =
                            thinking_in_context_est.saturating_add(estimate_tokens(th));
                    }
                }
                MessageRole::Tool => {
                    tool_tokens_est = tool_tokens_est.saturating_add(n);
                    tool_message_count = tool_message_count.saturating_add(1);
                }
                // `MessageRole` is `#[non_exhaustive]`. Count an unrecognised future role
                // toward the user bucket so the context budget total stays accurate instead
                // of silently under-counting the window.
                _ => user_tokens_est = user_tokens_est.saturating_add(n),
            }
        }
        let context_tokens_est = self.context_tokens_estimate();
        let context_capacity = self.context.config.capacity_tokens.max(1);
        let context_pct = (context_tokens_est as f64 / context_capacity as f64) * 100.0;
        TokenUsageReport {
            api: self.token_usage.clone(),
            context_tokens_est,
            context_capacity,
            context_pct,
            system_tokens_est,
            user_tokens_est,
            assistant_tokens_est,
            tool_tokens_est,
            thinking_in_context_est,
            tool_schema_tokens_est: self.tool_schema_tokens_estimate(),
            message_count: self.messages.len(),
            tool_message_count,
        }
    }

    /// Shared model client handle, usable without holding `&mut self`.
    pub fn model_client(&self) -> Arc<dyn ModelClient> {
        self.model.clone()
    }

    pub fn tool_registry(&self) -> Arc<ToolRegistry> {
        self.tools.clone()
    }

    /// Shared control handles for this session's background work. The owner
    /// (the repository supervisor) keeps a clone so cancellation and subagent
    /// approvals stay reachable while a running turn holds the session lock.
    pub fn background_control(&self) -> Arc<BackgroundControl> {
        self.tasks.background_control()
    }

    /// Active workspace root.
    pub fn workspace_root(&self) -> &std::path::Path {
        &self.tool_ctx.workspace_root
    }

    pub(crate) fn reset_turn_cancel(&mut self) {
        if self.turn_cancel_token.is_cancelled() {
            self.turn_cancel_token = tokio_util::sync::CancellationToken::new();
        }
    }

    /// Begin one cancellable root-turn scope and return the handle a session
    /// actor may retain while the session itself is owned by the running task.
    pub fn begin_turn_cancellation_scope(&mut self) -> tokio_util::sync::CancellationToken {
        self.reset_turn_cancel();
        self.turn_cancel_token.clone()
    }

    /// Use this provider/model id on subsequent completions (e.g. after `/connect`).
    pub fn set_active_model(&mut self, model: impl Into<String>) {
        let mut selection = self.base_model_selection();
        selection.model = model.into();
        self.set_model_selection(selection);
    }

    /// Fail-closed capability flag used to reject `view_image` at call time.
    pub fn set_image_input_supported(&mut self, supported: bool) {
        self.tool_ctx.image_input = supported;
    }

    /// Snapshot attachments into the session image cache and bake missing-file
    /// notes into `content`. The returned refs are what later requests resend.
    pub(crate) fn freeze_attachments(
        &self,
        content: &mut String,
        attachments: Vec<forge_types::ImageRef>,
    ) -> Vec<forge_types::ImageRef> {
        forge_model::freeze_attachments(
            &self.tool_ctx.workspace_root,
            &self.context.image_cache_dir(),
            content,
            attachments,
        )
    }

    pub(crate) fn freeze_tool_output(&self, output: &mut forge_types::ToolOutput) {
        output.attachments =
            self.freeze_attachments(&mut output.content, std::mem::take(&mut output.attachments));
    }

    pub fn image_input_supported(&self) -> bool {
        self.tool_ctx.image_input
    }

    #[cfg(test)]
    pub(crate) fn last_prompt_snapshot_for_tests(&self) -> (String, Vec<u8>) {
        (
            self.last_prompt_hash.clone().unwrap_or_default(),
            self.last_prompt_wire.clone().unwrap_or_default(),
        )
    }

    pub fn set_active_profile_id(&mut self, profile_id: impl Into<String>) {
        let mut selection = self.base_model_selection();
        selection.profile_id = profile_id.into();
        self.set_model_selection(selection);
    }

    /// Wire-level reasoning-effort value to send on the next completion, or
    /// `None` to omit the field (model doesn't support it, or effort is Auto).
    pub fn set_reasoning_effort(&mut self, effort: Option<String>) {
        let mut selection = self.base_model_selection();
        selection.reasoning_effort = effort;
        self.set_model_selection(selection);
    }

    pub fn reasoning_effort(&self) -> Option<&str> {
        self.reasoning_effort.as_deref()
    }

    pub fn set_thinking_enabled(&mut self, enabled: bool) {
        // Thinking is not part of a tier: the toggle applies now and after
        // a turn-scoped tier ends, and leaves the tier in place.
        self.set_base_thinking_enabled(enabled);
        let mut selection = self.model_selection();
        selection.thinking_enabled = enabled;
        self.set_model_selection_keeping_tier(selection);
    }

    pub fn thinking_enabled(&self) -> bool {
        self.thinking_enabled
    }

    /// Turn this session's approve-all mode on or off. While on, `Hitl`
    /// decisions are auto-approved and sandbox-denied hosts are granted for the
    /// session; shell stays confined and an unconfined retry still prompts.
    /// Session scoped; never persisted.
    ///
    /// Enabling is refused unless the workspace is trusted (see
    /// [`Self::set_workspace_trusted`]); disabling always applies. Returns
    /// whether the request took effect.
    pub fn set_approve_all(&mut self, on: bool) -> bool {
        let applied = on && self.workspace_trusted;
        self.approve_all
            .store(applied, std::sync::atomic::Ordering::Release);
        applied == on
    }

    /// Whether this session is a subagent: one run of a child started by a
    /// parent session, which carries that run's cancellation token.
    pub(crate) fn is_subagent(&self) -> bool {
        self.cancel_token.is_some()
    }

    pub fn approve_all(&self) -> bool {
        self.approve_all.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Record whether this session's workspace is trusted. Set once by the
    /// owner (the supervisor) from its trust store at session assembly.
    pub fn set_workspace_trusted(&mut self, trusted: bool) {
        self.workspace_trusted = trusted;
        if !trusted {
            self.approve_all
                .store(false, std::sync::atomic::Ordering::Release);
        }
    }

    pub fn workspace_trusted(&self) -> bool {
        self.workspace_trusted
    }

    /// Route the session's model requests through `table`, and strip its
    /// credential env names (with the built-in vendor aliases) from every
    /// child process the model triggers (shell, background jobs, git).
    pub fn set_route_table(&mut self, table: forge_model::RouteTable) {
        self.set_credential_env_names(table.credential_env_names().to_vec());
        self.model.set_route_table(table);
    }

    pub(crate) fn set_credential_env_names(&mut self, names: Vec<String>) {
        self.tool_ctx.credential_env_names = std::sync::Arc::from(names);
    }
}
