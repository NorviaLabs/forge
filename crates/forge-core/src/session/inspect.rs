//! Read-only reporting over a session, plus the small setters that
//! configure the next model call.
//!
//! Split out of `lib.rs`; methods are moved verbatim.

use crate::*;

impl AgentSession {
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
        if self.enable_gov {
            self.governance
                .filter_tools((*desc).clone())
                .into_iter()
                .map(|t| t.name)
                .collect()
        } else {
            self.tools.names()
        }
    }

    /// How many tools this session would expose to the model.
    ///
    /// Equivalent to `list_tools().len()`, without building the list. The
    /// status bar asks for this every frame, where materialising every
    /// descriptor — name, description and a cloned input schema apiece — was
    /// one of the larger per-frame allocation sources.
    pub fn tool_count(&self) -> usize {
        if self.enable_gov {
            self.tools
                .name_classes()
                .filter(|(name, class)| {
                    self.governance
                        .acl
                        .is_allowed(&self.governance.principal, name, *class)
                })
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

    pub async fn journal_cursor(&self) -> Result<u64, LoopError> {
        Ok(self.journal.last_seq().await?)
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

    pub fn token_usage_lines(&self) -> Vec<String> {
        let r = self.token_usage_report();
        let api = &r.api;
        let mut lines = vec![
            "Session token usage (not $)".to_string(),
            String::new(),
            "API-reported (cumulative)".to_string(),
            format!("  prompt/input tokens:      {}", api.prompt_tokens),
            format!("  completion/output tokens: {}", api.completion_tokens),
            format!("  total API tokens:         {}", api.total_api_tokens()),
            format!(
                "  model steps:              {} ({} with usage metadata)",
                api.model_steps, api.model_calls_with_usage
            ),
            format!("  thinking tokens (est.):   {}", api.thinking_tokens_est),
            String::new(),
            "In-context estimate (~4 chars/token)".to_string(),
            format!(
                "  total: {} / {}  ({:.1}% of capacity)",
                r.context_tokens_est, r.context_capacity, r.context_pct
            ),
            format!("  system:    {}", r.system_tokens_est),
            format!("  tool schemas: {}", r.tool_schema_tokens_est),
            format!("  user:      {}", r.user_tokens_est),
            format!("  assistant: {}", r.assistant_tokens_est),
            format!(
                "  tool:      {} ({} tool msgs)",
                r.tool_tokens_est, r.tool_message_count
            ),
            format!("  thinking:  {}", r.thinking_in_context_est),
            format!("  messages:  {}", r.message_count),
        ];
        if api.model_steps > 0 && api.model_calls_with_usage == 0 {
            lines.push(String::new());
            lines.push("Note: provider did not return usage; API totals may stay 0.".into());
        }
        lines
    }

    /// Shared model client handle (for streaming from the TUI without holding `&mut self`).
    pub fn model_client(&self) -> Arc<dyn ModelClient> {
        self.model.clone()
    }

    pub fn max_turns(&self) -> u32 {
        self.max_turns
    }

    /// Active workspace root.
    pub fn workspace_root(&self) -> &std::path::Path {
        &self.tool_ctx.workspace_root
    }

    /// Use this provider/model id on subsequent completions (e.g. after `/connect`).
    pub fn set_active_model(&mut self, model: impl Into<String>) {
        self.active_model = model.into();
        self.tool_ctx.active_model = self.active_model.clone();
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

    pub fn set_active_route_id(&mut self, route_id: impl Into<String>) {
        self.active_route_id = route_id.into();
    }

    /// Wire-level reasoning-effort value to send on the next completion, or
    /// `None` to omit the field (model doesn't support it, or effort is Auto).
    pub fn set_reasoning_effort(&mut self, effort: Option<String>) {
        self.reasoning_effort = effort;
    }

    /// Push provider credentials into the model client (OAuth tokens → worker env).
    pub fn apply_provider_env(&self, pairs: &[(String, String)]) {
        self.model.apply_provider_env(pairs);
    }

    /// Clear provider credentials from the model client and recycle the worker.
    pub fn clear_provider_env(&self) {
        self.model.clear_provider_env();
    }
}
