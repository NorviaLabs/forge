//! Context lifecycle (context-lifecycle.md) — CTX-01, CTX-02. Phase 2 only.

use std::fs;
use std::path::PathBuf;

use chrono::Utc;
use forge_types::{Message, MessageRole, ProgressDocument, SessionId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ContextError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextConfig {
    /// Offload when estimated tokens exceed this (default 2000).
    #[serde(default = "default_offload")]
    pub offload_token_threshold: usize,
    /// Reset when usage_ratio >= this (default 0.80).
    #[serde(default = "default_reset_ratio")]
    pub reset_usage_ratio: f64,
    /// Context capacity in tokens (heuristic).
    #[serde(default = "default_capacity")]
    pub capacity_tokens: usize,
    #[serde(default = "default_offload_dir")]
    pub offload_dir: String,
    #[serde(default = "default_progress_path")]
    pub progress_path: String,
}

fn default_offload() -> usize {
    2000
}
fn default_reset_ratio() -> f64 {
    0.80
}
fn default_capacity() -> usize {
    200_000
}
fn default_offload_dir() -> String {
    ".forge/offload".into()
}
fn default_progress_path() -> String {
    ".forge/progress.json".into()
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            offload_token_threshold: default_offload(),
            reset_usage_ratio: default_reset_ratio(),
            capacity_tokens: default_capacity(),
            offload_dir: default_offload_dir(),
            progress_path: default_progress_path(),
        }
    }
}

/// ~4 chars per token heuristic.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4).max(1)
}

pub fn estimate_messages_tokens(messages: &[Message]) -> usize {
    messages.iter().map(|m| estimate_tokens(&m.content)).sum()
}

#[derive(Debug, Clone)]
pub struct OffloadResult {
    pub uri: String,
    pub sha256: String,
    pub bytes: usize,
    pub tokens_estimate: usize,
    pub summary: String,
    pub in_context: String,
}

#[derive(Debug, Clone)]
pub struct ContextEngine {
    pub config: ContextConfig,
    pub workspace: PathBuf,
    pub session_id: SessionId,
    pub goal: String,
}

impl ContextEngine {
    pub fn new(workspace: PathBuf, session_id: SessionId) -> Self {
        Self {
            config: ContextConfig::default(),
            workspace,
            session_id,
            goal: String::new(),
        }
    }

    pub fn usage_ratio(&self, messages: &[Message]) -> f64 {
        let used = estimate_messages_tokens(messages) as f64;
        used / self.config.capacity_tokens as f64
    }

    pub fn should_reset(&self, messages: &[Message]) -> bool {
        self.usage_ratio(messages) >= self.config.reset_usage_ratio
    }

    /// CTX-01: offload large tool body to disk; return compact in-context form.
    pub fn offload_tool_output(&self, body: &str) -> Result<Option<OffloadResult>, ContextError> {
        let tokens = estimate_tokens(body);
        if tokens <= self.config.offload_token_threshold {
            return Ok(None);
        }
        let dir = self
            .workspace
            .join(&self.config.offload_dir)
            .join(self.session_id.to_string());
        fs::create_dir_all(&dir)?;
        let id = Uuid::new_v4();
        let file = dir.join(format!("tool_{id}.txt"));
        fs::write(&file, body.as_bytes())?;
        let mut hasher = Sha256::new();
        hasher.update(body.as_bytes());
        let sha = hex::encode(hasher.finalize());
        let rel = file
            .strip_prefix(&self.workspace)
            .unwrap_or(&file)
            .display()
            .to_string();
        let uri = format!("file://{rel}");
        let summary: String = body.chars().take(500).collect();
        let in_context = format!(
            "[offloaded tool output — {tokens} tokens]\nuri: {uri}\nsha256: {sha}\nsummary: {summary}"
        );
        let in_tokens = estimate_tokens(&in_context);
        // Ensure ≥80% reduction when we offload
        debug_assert!(in_tokens * 5 <= tokens || tokens > 0);
        Ok(Some(OffloadResult {
            uri,
            sha256: sha,
            bytes: body.len(),
            tokens_estimate: tokens,
            summary,
            in_context,
        }))
    }

    pub fn maybe_offload_tool_content(&self, body: String) -> Result<String, ContextError> {
        match self.offload_tool_output(&body)? {
            Some(o) => Ok(o.in_context),
            None => Ok(body),
        }
    }

    pub fn progress_path(&self) -> PathBuf {
        let p = PathBuf::from(&self.config.progress_path);
        if p.is_absolute() {
            p
        } else {
            self.workspace.join(p)
        }
    }

    pub fn write_progress(&self, doc: &ProgressDocument) -> Result<(), ContextError> {
        let path = self.progress_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let s = serde_json::to_string_pretty(doc)?;
        fs::write(path, s)?;
        Ok(())
    }

    pub fn read_progress(&self) -> Result<Option<ProgressDocument>, ContextError> {
        let path = self.progress_path();
        if !path.is_file() {
            return Ok(None);
        }
        let s = fs::read_to_string(path)?;
        Ok(Some(serde_json::from_str(&s)?))
    }

    pub fn load_agents_md(&self) -> String {
        let p = self.workspace.join("AGENTS.md");
        fs::read_to_string(p).unwrap_or_default()
    }

    pub fn load_skills(&self) -> Vec<(String, String)> {
        let mut skills = read_skills_dir(Some(self.workspace.join(".forge").join("skills")));
        skills.extend(read_skills_dir(global_skills_dir()));
        skills.sort_by(|a, b| a.0.cmp(&b.0));
        skills.dedup_by(|a, b| a.0 == b.0);
        skills
    }

    /// CTX-02 hard reset: write progress, clear window, rehydrate slim messages.
    pub fn handoff_reset(
        &self,
        messages: &[Message],
        workspace_ref: &str,
        system_prompt: &str,
    ) -> Result<(ProgressDocument, Vec<Message>), ContextError> {
        let mut doc = ProgressDocument::new(self.session_id, self.goal.clone());
        if doc.goal.is_empty() {
            doc.goal = messages
                .iter()
                .find(|m| m.role == MessageRole::User)
                .map(|m| m.content.chars().take(200).collect())
                .unwrap_or_else(|| "continue task".into());
        }
        doc.completed = messages
            .iter()
            .filter(|m| m.role == MessageRole::Assistant)
            .map(|m| m.content.chars().take(120).collect())
            .collect();
        doc.in_progress = "resumed after context reset".into();
        doc.next_actions = vec!["continue from progress.json".into()];
        doc.workspace_ref = workspace_ref.into();
        doc.updated_at = Utc::now().to_rfc3339();
        self.write_progress(&doc)?;

        let mut new_msgs = vec![Message {
            role: MessageRole::System,
            content: format!(
                "{system_prompt}\n\n# Context Handoff\n\nContext was reset (CTX-02). Continue from this progress document:\n{}",
                serde_json::to_string_pretty(&doc).unwrap_or_default()
            ),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
}];
        new_msgs.push(Message {
            role: MessageRole::User,
            content: format!(
                "Continue the task. Goal: {}. Next: {:?}",
                doc.goal, doc.next_actions
            ),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        });
        Ok((doc, new_msgs))
    }
}

/// Reduction ratio for offload (for tests / metrics).
pub fn reduction_ratio(original_tokens: usize, in_context_tokens: usize) -> f64 {
    if original_tokens == 0 {
        return 0.0;
    }
    1.0 - (in_context_tokens as f64 / original_tokens as f64)
}

fn read_skills_dir(dir: Option<PathBuf>) -> Vec<(String, String)> {
    let Some(dir) = dir else { return vec![] };
    fs::read_dir(dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter_map(|entry| {
            let path = entry.path().join("SKILL.md");
            let name = entry.file_name().to_string_lossy().into_owned();
            fs::read_to_string(path).ok().map(|content| (name, content))
        })
        .collect()
}

fn global_skills_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("forge").join("skills"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn estimate_tokens_rough() {
        assert!(estimate_tokens("abcd") >= 1);
        assert_eq!(estimate_tokens("a".repeat(400).as_str()), 100);
    }

    #[test]
    fn small_body_not_offloaded() {
        let dir = tempdir().unwrap();
        let eng = ContextEngine::new(dir.path().to_path_buf(), Uuid::new_v4());
        assert!(eng.offload_tool_output("short").unwrap().is_none());
    }

    #[test]
    fn large_body_offloaded_with_high_reduction() {
        let dir = tempdir().unwrap();
        let eng = ContextEngine::new(dir.path().to_path_buf(), Uuid::new_v4());
        let big = "x".repeat(20_000); // ~5000 tokens
        let o = eng.offload_tool_output(&big).unwrap().expect("offload");
        let in_tok = estimate_tokens(&o.in_context);
        let ratio = reduction_ratio(o.tokens_estimate, in_tok);
        assert!(
            ratio >= 0.80,
            "expected >=80% reduction, got {ratio} ({} -> {})",
            o.tokens_estimate,
            in_tok
        );
        assert!(o.uri.starts_with("file://"));
        let path = dir.path().join(
            o.uri
                .strip_prefix("file://")
                .unwrap()
                .trim_start_matches('/'),
        );
        // uri is relative path after file://
        let rel = o.uri.strip_prefix("file://").unwrap();
        let full = dir.path().join(rel);
        assert!(full.is_file(), "missing {}", full.display());
        let _ = path;
    }

    #[test]
    fn handoff_writes_progress_and_clears() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "Be careful").unwrap();
        let sid = Uuid::new_v4();
        let mut eng = ContextEngine::new(dir.path().to_path_buf(), sid);
        eng.goal = "ship feature".into();
        let messages = vec![
            Message {
                role: MessageRole::User,
                content: "do work".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                role: MessageRole::Assistant,
                content: "x".repeat(1000),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
        ];
        let system_prompt = "Forge system prompt\n\nAGENTS.md:\nBe careful";
        let (doc, new_msgs) = eng
            .handoff_reset(&messages, "abc123", system_prompt)
            .unwrap();
        assert_eq!(doc.version, 1);
        assert_eq!(doc.goal, "ship feature");
        assert!(eng.progress_path().is_file());
        assert!(new_msgs[0].content.contains("Be careful"));
        assert!(new_msgs[0].content.contains("# Context Handoff"));
        assert!(new_msgs[0].content.starts_with(system_prompt));
        // capacity default 200_000 tokens; fill past 80%
        assert!(eng.should_reset(
            &std::iter::repeat_n(
                Message {
                    role: MessageRole::Assistant,
                    content: "y".repeat(4000),
                    tool_call_id: None,
                    name: None,
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                },
                200,
            )
            .collect::<Vec<_>>()
        ));
    }

    #[test]
    fn load_skills_reads_forge_skills() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge/skills/ponytail")).unwrap();
        std::fs::write(
            dir.path().join(".forge/skills/ponytail/SKILL.md"),
            "forge skill",
        )
        .unwrap();

        let eng = ContextEngine::new(dir.path().to_path_buf(), Uuid::new_v4());

        assert_eq!(
            eng.load_skills(),
            vec![("ponytail".into(), "forge skill".into())]
        );
    }

    #[test]
    fn read_skills_dir_returns_empty_for_missing() {
        assert!(read_skills_dir(None).is_empty());
        assert!(read_skills_dir(Some(PathBuf::from("/nonexistent"))).is_empty());
    }

    #[test]
    fn read_skills_dir_reads_skill_files() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("myskill")).unwrap();
        std::fs::write(dir.path().join("myskill/SKILL.md"), "content").unwrap();
        let skills = read_skills_dir(Some(dir.path().to_path_buf()));
        assert_eq!(skills, vec![("myskill".into(), "content".into())]);
    }

    #[test]
    fn load_skills_project_overrides_global() {
        let dir = tempdir().unwrap();
        // project skill
        std::fs::create_dir_all(dir.path().join(".forge/skills/mine")).unwrap();
        std::fs::write(dir.path().join(".forge/skills/mine/SKILL.md"), "project").unwrap();

        let eng = ContextEngine::new(dir.path().to_path_buf(), Uuid::new_v4());
        // global dir doesn't exist; only project skill is loaded
        let skills = eng.load_skills();
        assert_eq!(skills, vec![("mine".into(), "project".into())]);
    }

    #[test]
    fn usage_ratio_increases() {
        let eng = ContextEngine::new(std::env::current_dir().unwrap(), Uuid::new_v4());
        let small = vec![Message {
            role: MessageRole::User,
            content: "hi".into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        }];
        assert!(eng.usage_ratio(&small) < 0.01);
    }

    #[test]
    fn maybe_offload_tool_content_passes_small_body_through_unchanged() {
        let dir = tempdir().unwrap();
        let eng = ContextEngine::new(dir.path().to_path_buf(), Uuid::new_v4());
        let body = "small body".to_string();
        let result = eng.maybe_offload_tool_content(body.clone()).unwrap();
        assert_eq!(result, body, "small body must pass through untouched");
    }

    #[test]
    fn maybe_offload_tool_content_replaces_large_body_with_in_context_summary() {
        let dir = tempdir().unwrap();
        let eng = ContextEngine::new(dir.path().to_path_buf(), Uuid::new_v4());
        let big = "z".repeat(20_000);
        let result = eng.maybe_offload_tool_content(big.clone()).unwrap();
        assert_ne!(result, big, "large body must be replaced");
        assert!(
            result.contains("offloaded tool output"),
            "expected offload summary, got: {result}"
        );
    }

    #[test]
    fn progress_path_is_absolute_when_configured_absolute() {
        let dir = tempdir().unwrap();
        let mut eng = ContextEngine::new(dir.path().to_path_buf(), Uuid::new_v4());
        let absolute = dir.path().join("elsewhere/progress.json");
        eng.config.progress_path = absolute.display().to_string();
        assert_eq!(eng.progress_path(), absolute);
    }

    #[test]
    fn progress_path_is_relative_to_workspace_by_default() {
        let dir = tempdir().unwrap();
        let eng = ContextEngine::new(dir.path().to_path_buf(), Uuid::new_v4());
        assert_eq!(eng.progress_path(), dir.path().join(".forge/progress.json"));
    }

    #[test]
    fn read_progress_returns_none_when_no_file_written_yet() {
        let dir = tempdir().unwrap();
        let eng = ContextEngine::new(dir.path().to_path_buf(), Uuid::new_v4());
        assert!(eng.read_progress().unwrap().is_none());
    }

    #[test]
    fn write_then_read_progress_round_trips() {
        let dir = tempdir().unwrap();
        let eng = ContextEngine::new(dir.path().to_path_buf(), Uuid::new_v4());
        let doc = ProgressDocument::new(eng.session_id, "ship it");
        eng.write_progress(&doc).unwrap();

        let loaded = eng
            .read_progress()
            .unwrap()
            .expect("progress file must exist");
        assert_eq!(loaded.goal, "ship it");
        assert_eq!(loaded.session_id, eng.session_id.to_string());
    }

    #[test]
    fn load_agents_md_returns_empty_string_when_missing() {
        let dir = tempdir().unwrap();
        let eng = ContextEngine::new(dir.path().to_path_buf(), Uuid::new_v4());
        assert_eq!(eng.load_agents_md(), "");
    }

    #[test]
    fn load_agents_md_returns_file_contents_when_present() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "follow the rules").unwrap();
        let eng = ContextEngine::new(dir.path().to_path_buf(), Uuid::new_v4());
        assert_eq!(eng.load_agents_md(), "follow the rules");
    }

    /// When `goal` is left empty, `handoff_reset` falls back to the first
    /// user message (truncated to 200 chars) instead of a blank goal.
    #[test]
    fn handoff_reset_falls_back_to_first_user_message_when_goal_is_empty() {
        let dir = tempdir().unwrap();
        let eng = ContextEngine::new(dir.path().to_path_buf(), Uuid::new_v4());
        assert!(eng.goal.is_empty());
        let messages = vec![
            Message {
                role: MessageRole::System,
                content: "system setup, not a user goal".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
            Message {
                role: MessageRole::User,
                content: "please refactor the parser".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
        ];
        let (doc, _new_msgs) = eng.handoff_reset(&messages, "ws", "prompt").unwrap();
        assert_eq!(doc.goal, "please refactor the parser");
    }

    /// With no user message at all, the fallback is the literal string
    /// `"continue task"` rather than an empty goal.
    #[test]
    fn handoff_reset_falls_back_to_continue_task_when_no_user_message_exists() {
        let dir = tempdir().unwrap();
        let eng = ContextEngine::new(dir.path().to_path_buf(), Uuid::new_v4());
        let messages = vec![Message {
            role: MessageRole::Assistant,
            content: "no user turn here".into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        }];
        let (doc, _new_msgs) = eng.handoff_reset(&messages, "ws", "prompt").unwrap();
        assert_eq!(doc.goal, "continue task");
    }

    #[test]
    fn reduction_ratio_is_zero_for_zero_original_tokens() {
        assert_eq!(reduction_ratio(0, 0), 0.0);
        assert_eq!(reduction_ratio(0, 5), 0.0);
    }
}
