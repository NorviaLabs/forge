//! Custom agents inside a session.
//!
//! The definitions themselves (files, parsing, scopes, capability rules) live
//! in `forge_config::agents`. This module is what a session does with them:
//!
//! - lists the available agents, by name, capabilities and description, in
//!   the top-level system prompt (`# Agents`), and keeps that list current
//!   as agents are saved, edited and deleted;
//! - turns a `spawn_agent` call — with or without `agent` — into a
//!   [`SubagentSpec`], re-reading the agent's file so an edit applies to the
//!   next start;
//! - describes what a child was started as ([`ChildProfile`]), which is
//!   applied to the child and journaled in the child's own journal so a
//!   resumed child keeps its instructions, tool limit, network and model;
//! - validates a caller-chosen model and reasoning effort through a
//!   [`ChildModelResolver`] the session's owner installs.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use forge_config::agents::{self, AgentCatalog, AgentDirs, LoadedAgent};
use forge_tools::{AgentMode, SpawnAgentArgs, ToolRegistry};
use forge_types::SideEffectClass;
use serde::{Deserialize, Serialize};

use crate::{AgentSession, LoopError, ModelExecutionSelection, SubagentSpec};

/// Reasoning-effort names accepted when no resolver knows the model's own
/// levels.
const EFFORT_LEVELS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max"];

/// The model a child agent runs on, when the caller chose one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildModel {
    pub model: String,
    pub profile_id: String,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// The model's context window and output reserve, when the catalog
    /// knows them.
    #[serde(default)]
    pub context_window: Option<usize>,
    #[serde(default)]
    pub output_reserve: Option<usize>,
    #[serde(default)]
    pub image_input: Option<bool>,
}

/// What a child was started as, beyond its label and first prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildProfile {
    /// The custom agent's name, when started as one.
    #[serde(default)]
    pub agent: Option<String>,
    /// The agent's capabilities as written in its file (`read-only`,
    /// `write, exec`, …).
    #[serde(default)]
    pub capabilities: Option<String>,
    /// The agent's instructions, added to the child's system prompt.
    #[serde(default)]
    pub instructions: Option<String>,
    /// Withhold the session's network gateway from the child's sandboxed
    /// commands.
    #[serde(default)]
    pub no_network: bool,
    /// A model or effort the caller chose; `None` inherits the parent's.
    #[serde(default)]
    pub model: Option<ChildModel>,
}

/// The `AgentProfile` journal payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct JournaledProfile {
    #[serde(default)]
    pub tool_limit: Option<Vec<String>>,
    #[serde(default)]
    pub mode: Option<AgentMode>,
    #[serde(flatten)]
    pub profile: ChildProfile,
}

/// A caller's model and effort request for a child agent.
#[derive(Debug, Clone, Copy)]
pub struct ChildModelRequest<'a> {
    pub model: Option<&'a str>,
    pub reasoning_effort: Option<&'a str>,
    /// The spawning session's own selection, used for whatever the request
    /// leaves out.
    pub parent: &'a ModelExecutionSelection,
}

/// Validates a child's model and effort against the providers and the model
/// catalog. Installed by the session's owner (`forge-session`), which knows
/// them; without one, only the shape of the request is checked.
pub trait ChildModelResolver: Send + Sync {
    fn resolve(&self, request: &ChildModelRequest<'_>) -> Result<ChildModel, String>;
}

/// Shape-only resolution: `<provider>/<model>` and a known effort name.
pub fn resolve_child_model_shape(request: &ChildModelRequest<'_>) -> Result<ChildModel, String> {
    let (model, profile_id) = match request.model {
        Some(model) => {
            let model = model.trim();
            let provider = model
                .split_once('/')
                .filter(|(provider, bare)| !provider.is_empty() && !bare.is_empty())
                .map(|(provider, _)| provider.to_string())
                .ok_or_else(|| format!("model `{model}` must be written `<provider>/<model>`"))?;
            (model.to_string(), provider)
        }
        None => (
            request.parent.model.clone(),
            request.parent.profile_id.clone(),
        ),
    };
    let reasoning_effort = match request.reasoning_effort {
        Some(effort) => {
            let effort = effort.trim().to_ascii_lowercase();
            if !EFFORT_LEVELS.contains(&effort.as_str()) {
                return Err(format!(
                    "reasoning effort `{effort}` is not one of {}",
                    EFFORT_LEVELS.join(", ")
                ));
            }
            Some(effort)
        }
        // A different model starts on its own default; the same model keeps
        // the parent's effort.
        None if request.model.is_some() => None,
        None => request.parent.reasoning_effort.clone(),
    };
    Ok(ChildModel {
        model,
        profile_id,
        reasoning_effort,
        context_window: None,
        output_reserve: None,
        image_input: None,
    })
}

/// The agents a top-level session offers, and what is wrong with the rest.
#[derive(Debug, Clone, Default)]
pub(crate) struct CustomAgentsLoad {
    pub agents: Vec<LoadedAgent>,
    pub warnings: Vec<String>,
    /// The project root the agents were read for, kept so a later refresh
    /// does not run `git` again.
    pub project_root: PathBuf,
    /// The `# Agents` text for the system prompt; empty without agents.
    pub section: String,
}

/// The agent directories for a project root and the user directory the
/// session was given. The root is the main checkout even for a session in a
/// linked worktree, so an agent saved in Settings reaches every session of
/// the project.
pub(crate) fn agent_dirs(project_root: &Path, user_dir: Option<&Path>) -> AgentDirs {
    AgentDirs {
        project: Some(agents::project_agents_dir(project_root)),
        user: user_dir.map(Path::to_path_buf),
    }
}

fn available_tools(tools: &ToolRegistry) -> Vec<(String, SideEffectClass)> {
    tools
        .list_descriptors()
        .iter()
        .map(|descriptor| (descriptor.name.clone(), descriptor.side_effect_class))
        .collect()
}

/// Discover the agents for the project `workspace` belongs to (and in
/// `user_dir`, if any) and check their tools against `tools`. Never fails:
/// every problem becomes a warning.
pub(crate) fn load_custom_agents(
    workspace: &Path,
    user_dir: Option<&Path>,
    tools: &ToolRegistry,
) -> CustomAgentsLoad {
    load_custom_agents_at(&forge_storage::project_root(workspace), user_dir, tools)
}

/// [`load_custom_agents`] for a project root already known.
pub(crate) fn load_custom_agents_at(
    project_root: &Path,
    user_dir: Option<&Path>,
    tools: &ToolRegistry,
) -> CustomAgentsLoad {
    let catalog = agent_dirs(project_root, user_dir).discover();
    let available = available_tools(tools);
    let mut warnings = catalog.issue_messages();
    for agent in &catalog.agents {
        let resolution = agents::resolve_agent_tools(&agent.definition, &available);
        warnings.extend(resolution.warnings);
        if resolution.allowed.is_empty() {
            warnings.push(format!(
                "agent `{}`: none of its tools are available; it can only reply with text",
                agent.definition.name
            ));
        }
    }
    let section = agents_section(&catalog.agents);
    CustomAgentsLoad {
        agents: catalog.agents,
        warnings,
        project_root: project_root.to_path_buf(),
        section,
    }
}

/// Append the `# Agents` section for a top-level session. Nothing is added
/// when there are no agents.
pub(crate) fn append_agents_section(prompt: &mut String, agents: &[LoadedAgent]) {
    prompt.push_str(&agents_section(agents));
}

/// The `# Agents` section: empty without agents.
pub(crate) fn agents_section(agents: &[LoadedAgent]) -> String {
    let mut prompt = String::new();
    if agents.is_empty() {
        return prompt;
    }
    prompt.push_str(
        "\n\n# Agents\n\nThe user configured these custom agents. Each is listed by name, \
capabilities and description. When a task matches an agent's description, delegate it with \
`spawn_agent`, passing `agent` with the agent's name and a self-contained `message`; the \
agent's own instructions, capabilities and tools apply. If the user asks for a specific model \
or reasoning effort for the agent, pass `model` (`<provider>/<model>`) and `reasoning_effort`.",
    );
    for agent in agents {
        let definition = &agent.definition;
        prompt.push_str("\n\n## ");
        prompt.push_str(&definition.name);
        prompt.push_str("\n\nCapabilities: ");
        prompt.push_str(&definition.mode_label());
        prompt.push('\n');
        prompt.push_str(definition.description.trim());
    }
    prompt
}

/// Append the role section a named agent's child runs under.
pub(crate) fn append_role_section(prompt: &mut String, profile: &ChildProfile) {
    let Some(agent) = profile.agent.as_deref() else {
        return;
    };
    prompt.push_str("\n\n# Your role\n\nYou are running as the custom agent `");
    prompt.push_str(agent);
    prompt.push('`');
    if let Some(capabilities) = profile.capabilities.as_deref() {
        prompt.push_str(" (capabilities: ");
        prompt.push_str(capabilities);
        prompt.push(')');
    }
    prompt.push_str(". Only the tools you were given are available to you.");
    if profile.no_network {
        prompt.push_str(" Commands you run have no network access.");
    }
    if let Some(instructions) = profile
        .instructions
        .as_deref()
        .map(str::trim)
        .filter(|instructions| !instructions.is_empty())
    {
        prompt.push_str(" Follow the agent's instructions:\n\n");
        prompt.push_str(instructions);
    }
}

fn unknown_agent(name: &str, catalog: &AgentCatalog) -> LoopError {
    let file_name = format!("{name}.md");
    if let Some(issue) = catalog
        .issues
        .iter()
        .find(|issue| issue.file_name == file_name)
    {
        return LoopError::Other(format!(
            "the custom agent `{name}` did not load ({}): {}",
            issue.path.display(),
            issue.reason
        ));
    }
    let names = catalog.names();
    LoopError::Other(if names.is_empty() {
        format!("there is no custom agent named `{name}`; no custom agents are configured")
    } else {
        format!(
            "there is no custom agent named `{name}`; available agents: {}",
            names.join(", ")
        )
    })
}

impl AgentSession {
    /// Warnings from loading the custom agents: files that were skipped, and
    /// tools an agent lists that it cannot have. Empty for a child agent.
    pub fn custom_agent_warnings(&self) -> Vec<String> {
        self.custom_agent_warnings.as_ref().clone()
    }

    /// What this session was started as, when it is a child agent.
    pub fn child_profile(&self) -> Option<&ChildProfile> {
        self.child_profile.as_ref()
    }

    /// The tools this session is limited to, when it is a child agent
    /// started with a tool list.
    pub fn tool_limit(&self) -> Option<Vec<String>> {
        self.tool_limit
            .as_ref()
            .map(|limit| limit.iter().cloned().collect())
    }

    /// Install the resolver that validates a caller-chosen child model.
    pub fn set_child_model_resolver(&mut self, resolver: Arc<dyn ChildModelResolver>) {
        self.child_model_resolver = Some(resolver);
    }

    /// Re-read the custom agents: the warnings a frontend shows, and the
    /// `# Agents` list in the system prompt, so an agent saved, edited or
    /// deleted mid-session is what the model sees from its next request. The
    /// prompt is rewritten only when the list actually changed, so an
    /// unchanged list costs no prompt-cache miss. Runs before every user
    /// message, and when Settings saves or deletes an agent.
    pub fn refresh_custom_agents(&mut self) {
        if self.is_subagent() {
            return;
        }
        let project_root = self
            .agents_project_root
            .clone()
            .unwrap_or_else(|| forge_storage::project_root(&self.tool_ctx.workspace_root));
        let loaded =
            load_custom_agents_at(&project_root, self.user_agents_dir.as_deref(), &self.tools);
        self.set_custom_agents_loaded(&loaded);
    }

    /// Take a fresh load: its warnings and project root, and its `# Agents`
    /// text, swapped into the end of the system prompt if it changed.
    pub(crate) fn set_custom_agents_loaded(&mut self, loaded: &CustomAgentsLoad) {
        self.custom_agent_warnings = Arc::new(loaded.warnings.clone());
        self.agents_project_root = Some(loaded.project_root.clone());
        if loaded.section == self.agents_section {
            return;
        }
        let Some(system) = self
            .messages
            .first()
            .filter(|message| message.role == forge_types::MessageRole::System)
        else {
            return;
        };
        // The section is always the end of the system prompt; anything else
        // means the prompt was not built here, and it is left alone.
        let Some(base) = system.content.strip_suffix(self.agents_section.as_str()) else {
            tracing::warn!("system prompt does not end with its agents section; not updated");
            return;
        };
        let content = format!("{base}{}", loaded.section);
        self.messages[0].content = content;
        // The cached context-size estimate only notices changes at the end of
        // the transcript.
        if let Ok(mut cache) = self.ctx_tokens_cache.lock() {
            *cache = None;
        }
        self.agents_section = loaded.section.clone();
    }

    /// Whether the tool limit, if any, lets this session call `tool`.
    pub(crate) fn tool_within_limit(&self, tool: &str) -> bool {
        self.tool_limit
            .as_ref()
            .is_none_or(|limit| limit.contains(tool))
    }

    pub(crate) fn set_tool_limit(&mut self, tools: Option<Vec<String>>) {
        self.tool_limit = tools.map(|tools| Arc::new(tools.into_iter().collect::<BTreeSet<_>>()));
    }

    /// Put a child on the model its caller chose.
    pub(crate) fn apply_child_model(&mut self, model: &ChildModel) {
        let mut selection = self.model_selection();
        selection.model = model.model.clone();
        selection.profile_id = model.profile_id.clone();
        selection.reasoning_effort = model.reasoning_effort.clone();
        self.set_model_selection(selection);
        if let Some(window) = model.context_window {
            self.set_context_window(window, model.output_reserve);
        }
        if let Some(image_input) = model.image_input {
            self.set_image_input_supported(image_input);
        }
    }

    /// Apply everything a child was started as. Used when it is created and
    /// again when it is resumed from its journal.
    pub(crate) fn apply_child_profile(
        &mut self,
        tool_limit: Option<Vec<String>>,
        profile: &ChildProfile,
    ) {
        self.set_tool_limit(tool_limit);
        if profile.no_network {
            self.tool_ctx.egress = None;
        }
        if let Some(model) = &profile.model {
            self.apply_child_model(model);
        }
        self.child_profile = Some(profile.clone());
    }

    /// Turn a `spawn_agent` call into what to start. A named agent's file is
    /// read now, so the latest saved version is the one that runs.
    pub(crate) fn subagent_spec_for(
        &self,
        args: SpawnAgentArgs,
    ) -> Result<SubagentSpec, LoopError> {
        let nonempty = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        let requested_model = nonempty(&args.model);
        let requested_effort = nonempty(&args.reasoning_effort);
        let model = if requested_model.is_some() || requested_effort.is_some() {
            let parent = self.model_selection();
            let request = ChildModelRequest {
                model: requested_model.as_deref(),
                reasoning_effort: requested_effort.as_deref(),
                parent: &parent,
            };
            let resolved = match &self.child_model_resolver {
                Some(resolver) => resolver.resolve(&request),
                None => resolve_child_model_shape(&request),
            };
            Some(resolved.map_err(LoopError::Other)?)
        } else {
            None
        };

        let Some(name) = nonempty(&args.agent) else {
            let role = args.label().ok_or_else(|| {
                LoopError::Other(
                    "`task_name` is required unless `agent` names a custom agent".into(),
                )
            })?;
            return Ok(SubagentSpec {
                role,
                prompt: args.message,
                mode: args.mode,
                tool_allowlist: args.tool_allowlist,
                profile: ChildProfile {
                    model,
                    ..ChildProfile::default()
                },
            });
        };

        let project_root = self
            .agents_project_root
            .clone()
            .unwrap_or_else(|| forge_storage::project_root(&self.tool_ctx.workspace_root));
        let catalog = agent_dirs(&project_root, self.user_agents_dir.as_deref()).discover();
        let agent = catalog
            .get(&name)
            .ok_or_else(|| unknown_agent(&name, &catalog))?;
        let definition = &agent.definition;
        let mut tools =
            agents::resolve_agent_tools(definition, &available_tools(&self.tools)).allowed;
        if let Some(narrower) = &args.tool_allowlist {
            tools.retain(|tool| narrower.contains(tool));
        }
        Ok(SubagentSpec {
            role: args.label().unwrap_or_else(|| definition.name.clone()),
            prompt: args.message,
            mode: if definition.needs_worktree() {
                AgentMode::Writer
            } else {
                AgentMode::ReadOnly
            },
            tool_allowlist: Some(tools),
            profile: ChildProfile {
                agent: Some(definition.name.clone()),
                capabilities: Some(definition.mode_label()),
                instructions: Some(definition.instructions.clone())
                    .filter(|instructions| !instructions.trim().is_empty()),
                no_network: !definition.network(),
                model,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent() -> ModelExecutionSelection {
        ModelExecutionSelection {
            model: "openai/gpt-parent".into(),
            profile_id: "openai".into(),
            reasoning_effort: Some("medium".into()),
            thinking_enabled: true,
        }
    }

    #[test]
    fn shape_resolution_checks_the_model_id_and_effort_name() {
        let parent = parent();
        let request = |model, effort| ChildModelRequest {
            model,
            reasoning_effort: effort,
            parent: &parent,
        };
        let chosen =
            resolve_child_model_shape(&request(Some("anthropic/claude-x"), Some("HIGH"))).unwrap();
        assert_eq!(chosen.model, "anthropic/claude-x");
        assert_eq!(chosen.profile_id, "anthropic");
        assert_eq!(chosen.reasoning_effort.as_deref(), Some("high"));

        // Only an effort: the parent's model, the new effort.
        let effort_only = resolve_child_model_shape(&request(None, Some("low"))).unwrap();
        assert_eq!(effort_only.model, "openai/gpt-parent");
        assert_eq!(effort_only.reasoning_effort.as_deref(), Some("low"));

        // Only a model: that model's own default effort, not the parent's.
        let model_only = resolve_child_model_shape(&request(Some("xai/grok"), None)).unwrap();
        assert_eq!(model_only.reasoning_effort, None);

        for bad in ["claude", "/claude", "anthropic/"] {
            let error = resolve_child_model_shape(&request(Some(bad), None)).unwrap_err();
            assert!(error.contains("<provider>/<model>"), "{bad}: {error}");
        }
        let error = resolve_child_model_shape(&request(None, Some("extreme"))).unwrap_err();
        assert!(error.contains("not one of"), "{error}");
    }

    #[test]
    fn the_agents_section_lists_name_capabilities_and_description() {
        let agent = |name: &str, mode| LoadedAgent {
            definition: agents::AgentDefinition {
                name: name.into(),
                description: format!("Does {name} things."),
                mode,
                tools: vec!["read_file".into()],
                instructions: "secret instructions".into(),
            },
            scope: agents::AgentScope::Project,
            path: format!("/p/.forge/agents/{name}.md").into(),
            file_name: format!("{name}.md"),
        };
        let mut prompt = String::from("base");
        append_agents_section(&mut prompt, &[]);
        assert_eq!(prompt, "base", "no agents, no section");

        append_agents_section(
            &mut prompt,
            &[
                agent("code-reviewer", vec![agents::Capability::ReadOnly]),
                agent(
                    "test-fixer",
                    vec![agents::Capability::Write, agents::Capability::Exec],
                ),
            ],
        );
        assert!(prompt.contains("# Agents"));
        assert!(prompt.contains("`spawn_agent`"));
        assert!(prompt
            .contains("## code-reviewer\n\nCapabilities: read-only\nDoes code-reviewer things."));
        assert!(prompt.contains("## test-fixer\n\nCapabilities: write, exec\n"));
        assert!(
            !prompt.contains("secret instructions"),
            "only name and description are disclosed up front"
        );
    }

    #[test]
    fn the_role_section_carries_the_instructions_and_network_note() {
        let mut prompt = String::new();
        append_role_section(&mut prompt, &ChildProfile::default());
        assert!(prompt.is_empty(), "an unnamed child has no role section");

        append_role_section(
            &mut prompt,
            &ChildProfile {
                agent: Some("test-fixer".into()),
                capabilities: Some("write, exec".into()),
                instructions: Some("Fix the failing tests.".into()),
                no_network: true,
                model: None,
            },
        );
        assert!(prompt.contains("custom agent `test-fixer` (capabilities: write, exec)"));
        assert!(prompt.contains("no network access"));
        assert!(prompt.ends_with("Fix the failing tests."));
    }

    #[test]
    fn the_journaled_profile_round_trips() {
        let journaled = JournaledProfile {
            tool_limit: Some(vec!["read_file".into()]),
            mode: Some(AgentMode::ReadOnly),
            profile: ChildProfile {
                agent: Some("code-reviewer".into()),
                capabilities: Some("read-only".into()),
                instructions: Some("Review.".into()),
                no_network: true,
                model: Some(ChildModel {
                    model: "anthropic/claude-x".into(),
                    profile_id: "anthropic".into(),
                    reasoning_effort: Some("high".into()),
                    context_window: Some(200_000),
                    output_reserve: Some(8_000),
                    image_input: Some(true),
                }),
            },
        };
        let value = serde_json::to_value(&journaled).unwrap();
        assert_eq!(value["agent"], "code-reviewer");
        let back: JournaledProfile = serde_json::from_value(value).unwrap();
        assert_eq!(back, journaled);
        // An older or minimal payload still reads.
        let minimal: JournaledProfile = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(minimal.profile, ChildProfile::default());
    }
}
