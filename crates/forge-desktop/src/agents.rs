//! Custom agents for Settings: list, create, edit, move, repair and delete
//! the agent files of the project on screen and of the user.
//!
//! Files are read and written only through `forge_config::agents`, which
//! validates every save and never touches a path the page names: the page
//! identifies a file by scope and file name, and the directory comes from
//! here. Writing a project's agents needs the project to be trusted, the same
//! rule as everything else Forge writes into a project.

use std::path::PathBuf;

use forge_config::agents::{
    AgentCatalog, AgentDefinition, AgentDirs, AgentFileIssue, AgentRef, AgentScope, Capability,
    LoadedAgent, NEVER_DELEGATED_TOOLS,
};
use forge_types::SideEffectClass;
use serde::{Deserialize, Serialize};

/// The project on screen, as far as agents are concerned.
#[derive(Debug, Clone)]
pub struct ProjectScope {
    pub name: String,
    pub root: PathBuf,
    pub trusted: bool,
}

impl ProjectScope {
    pub fn dirs(&self) -> AgentDirs {
        AgentDirs::for_project(Some(&self.root))
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentView {
    pub name: String,
    pub description: String,
    pub mode: Vec<String>,
    pub tools: Vec<String>,
    pub instructions: String,
    pub scope: AgentScope,
    pub file_name: String,
    pub path: String,
    /// A project agent hiding a user agent of the same name.
    pub overrides_user: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentIssueView {
    pub scope: AgentScope,
    pub file_name: String,
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentsView {
    pub project_name: String,
    pub project_trusted: bool,
    /// Where each scope's files live, shown so the user knows.
    pub project_dir: String,
    pub user_dir: String,
    /// The effective agents, sorted by name.
    pub agents: Vec<AgentView>,
    /// User agents a project agent overrides.
    pub overridden: Vec<AgentView>,
    /// Files that did not load.
    pub issues: Vec<AgentIssueView>,
}

/// What the editor sends. Every field of an agent is editable.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentInput {
    pub name: String,
    pub description: String,
    pub mode: Vec<String>,
    pub tools: Vec<String>,
    #[serde(default)]
    pub instructions: String,
}

/// One tool the editor's checklist offers, with the capability it needs.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentToolView {
    pub name: String,
    /// `read`, `write`, `exec`, `network` or `meta`.
    pub class: String,
    pub description: String,
}

fn view(agent: &LoadedAgent, overrides_user: bool) -> AgentView {
    let definition = &agent.definition;
    AgentView {
        name: definition.name.clone(),
        description: definition.description.clone(),
        mode: definition
            .mode
            .iter()
            .map(|capability| capability.as_str().to_string())
            .collect(),
        tools: definition.tools.clone(),
        instructions: definition.instructions.clone(),
        scope: agent.scope,
        file_name: agent.file_name.clone(),
        path: agent.path.display().to_string(),
        overrides_user,
    }
}

fn issue_view(issue: &AgentFileIssue) -> AgentIssueView {
    AgentIssueView {
        scope: issue.scope,
        file_name: issue.file_name.clone(),
        path: issue.path.display().to_string(),
        reason: issue.reason.clone(),
    }
}

fn catalog_view(project: &ProjectScope, dirs: &AgentDirs, catalog: &AgentCatalog) -> AgentsView {
    let overridden_names: Vec<&str> = catalog
        .overridden
        .iter()
        .map(|agent| agent.definition.name.as_str())
        .collect();
    let dir = |dir: &Option<PathBuf>| {
        dir.as_ref()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default()
    };
    AgentsView {
        project_name: project.name.clone(),
        project_trusted: project.trusted,
        project_dir: dir(&dirs.project),
        user_dir: dir(&dirs.user),
        agents: catalog
            .agents
            .iter()
            .map(|agent| {
                let overrides = agent.scope == AgentScope::Project
                    && overridden_names.contains(&agent.definition.name.as_str());
                view(agent, overrides)
            })
            .collect(),
        overridden: catalog
            .overridden
            .iter()
            .map(|agent| view(agent, false))
            .collect(),
        issues: catalog.issues.iter().map(issue_view).collect(),
    }
}

pub fn list(project: &ProjectScope, dirs: &AgentDirs) -> AgentsView {
    catalog_view(project, dirs, &dirs.discover())
}

/// Project files are written only in a trusted project.
fn may_write(project: &ProjectScope, scope: AgentScope) -> Result<(), String> {
    if scope == AgentScope::Project && !project.trusted {
        return Err(format!(
            "Trust {} before saving project agents: Forge writes nothing into a project you have not trusted.",
            project.name
        ));
    }
    Ok(())
}

fn definition(input: AgentInput) -> Result<AgentDefinition, String> {
    let mode = input
        .mode
        .iter()
        .map(|value| {
            Capability::parse(value).ok_or_else(|| format!("`{value}` is not a capability"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AgentDefinition {
        name: input.name,
        description: input.description,
        mode,
        tools: input.tools,
        instructions: input.instructions,
    })
}

/// Create an agent, or edit the one `previous` names: any field, its name,
/// and its scope. Returns the saved agent as it now reads from disk.
pub fn save(
    project: &ProjectScope,
    dirs: &AgentDirs,
    input: AgentInput,
    scope: AgentScope,
    previous: Option<AgentRef>,
) -> Result<AgentView, String> {
    may_write(project, scope)?;
    if let Some(previous) = &previous {
        may_write(project, previous.scope)?;
    }
    let definition = definition(input)?;
    let saved = dirs
        .save(&definition, scope, previous.as_ref())
        .map_err(|error| error.to_string())?;
    Ok(view(&saved, false))
}

/// Save an agent file's raw text, to repair one that did not load.
pub fn save_source(
    project: &ProjectScope,
    dirs: &AgentDirs,
    scope: AgentScope,
    previous: Option<AgentRef>,
    text: &str,
) -> Result<AgentView, String> {
    may_write(project, scope)?;
    if let Some(previous) = &previous {
        may_write(project, previous.scope)?;
    }
    let saved = dirs
        .save_source(scope, previous.as_ref(), text)
        .map_err(|error| error.to_string())?;
    Ok(view(&saved, false))
}

pub fn read_source(dirs: &AgentDirs, agent: &AgentRef) -> Result<String, String> {
    dirs.read_source(agent).map_err(|error| error.to_string())
}

pub fn delete(project: &ProjectScope, dirs: &AgentDirs, agent: &AgentRef) -> Result<(), String> {
    may_write(project, agent.scope)?;
    dirs.delete(agent).map_err(|error| error.to_string())
}

fn class_name(class: SideEffectClass) -> String {
    serde_json::to_value(class)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "meta".into())
}

/// The built-in tools an agent can be given, with their classes. MCP tools
/// depend on the project's servers; the editor takes their names as text.
pub fn tools() -> Vec<AgentToolView> {
    let mut tools: Vec<AgentToolView> = forge_tools::default_builtins()
        .iter()
        .filter(|tool| !NEVER_DELEGATED_TOOLS.contains(&tool.name()))
        .map(|tool| AgentToolView {
            name: tool.name().to_string(),
            class: class_name(tool.side_effect_class()),
            description: tool.description().to_string(),
        })
        .collect();
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary directory removed when dropped; the desktop crate has no
    /// `tempfile` dependency.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "forge-desktop-agents-{label}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn setup(label: &str, trusted: bool) -> (Scratch, ProjectScope, AgentDirs) {
        let scratch = Scratch::new(label);
        let project = ProjectScope {
            name: "demo".into(),
            root: scratch.0.join("demo"),
            trusted,
        };
        let dirs = AgentDirs {
            project: Some(project.root.join(".forge/agents")),
            user: Some(scratch.0.join("user-agents")),
        };
        (scratch, project, dirs)
    }

    fn input(name: &str, mode: &[&str], tools: &[&str]) -> AgentInput {
        AgentInput {
            name: name.into(),
            description: format!("The {name} agent."),
            mode: mode.iter().map(|value| value.to_string()).collect(),
            tools: tools.iter().map(|value| value.to_string()).collect(),
            instructions: "Do the job.".into(),
        }
    }

    #[test]
    fn saving_loading_editing_and_deleting_through_the_settings_surface() {
        let (_scratch, project, dirs) = setup("crud", true);
        let saved = save(
            &project,
            &dirs,
            input("reviewer", &["read-only"], &["read_file", "grep"]),
            AgentScope::Project,
            None,
        )
        .unwrap();
        assert_eq!(saved.file_name, "reviewer.md");
        assert!(saved.path.ends_with("demo/.forge/agents/reviewer.md"));

        let listed = list(&project, &dirs);
        assert_eq!(listed.project_name, "demo");
        assert!(listed.project_dir.ends_with("demo/.forge/agents"));
        assert_eq!(listed.agents.len(), 1);
        assert_eq!(listed.agents[0].mode, vec!["read-only"]);
        assert_eq!(listed.agents[0].instructions, "Do the job.");

        // Every field changes, including the name and the scope.
        let mut edited = input("auditor", &["write", "exec"], &["edit", "bash"]);
        edited.description = "Audits and fixes.".into();
        edited.instructions = "New instructions.".into();
        let moved = save(
            &project,
            &dirs,
            edited,
            AgentScope::User,
            Some(AgentRef {
                scope: AgentScope::Project,
                file_name: "reviewer.md".into(),
            }),
        )
        .unwrap();
        assert_eq!(moved.scope, AgentScope::User);
        let listed = list(&project, &dirs);
        assert_eq!(listed.agents.len(), 1);
        let agent = &listed.agents[0];
        assert_eq!(
            (agent.name.as_str(), agent.scope, agent.description.as_str()),
            ("auditor", AgentScope::User, "Audits and fixes.")
        );
        assert_eq!(agent.mode, vec!["write", "exec"]);
        assert_eq!(agent.tools, vec!["edit", "bash"]);
        assert_eq!(agent.instructions, "New instructions.");

        delete(
            &project,
            &dirs,
            &AgentRef {
                scope: AgentScope::User,
                file_name: "auditor.md".into(),
            },
        )
        .unwrap();
        assert!(list(&project, &dirs).agents.is_empty());
    }

    #[test]
    fn an_override_is_flagged_on_both_sides() {
        let (_scratch, project, dirs) = setup("override", true);
        for scope in [AgentScope::User, AgentScope::Project] {
            save(
                &project,
                &dirs,
                input("reviewer", &["read-only"], &["grep"]),
                scope,
                None,
            )
            .unwrap();
        }
        let listed = list(&project, &dirs);
        assert_eq!(listed.agents.len(), 1);
        assert_eq!(listed.agents[0].scope, AgentScope::Project);
        assert!(listed.agents[0].overrides_user);
        assert_eq!(listed.overridden.len(), 1);
        assert_eq!(listed.overridden[0].scope, AgentScope::User);
    }

    #[test]
    fn an_untrusted_project_gets_no_project_agents_written() {
        let (_scratch, project, dirs) = setup("untrusted", false);
        let error = save(
            &project,
            &dirs,
            input("reviewer", &["read-only"], &["grep"]),
            AgentScope::Project,
            None,
        )
        .unwrap_err();
        assert!(error.contains("Trust demo"), "{error}");
        assert!(!project.root.exists(), "nothing was written");
        // User agents do not depend on the project.
        save(
            &project,
            &dirs,
            input("reviewer", &["read-only"], &["grep"]),
            AgentScope::User,
            None,
        )
        .unwrap();
        assert!(!list(&project, &dirs).project_trusted);
    }

    #[test]
    fn invalid_input_is_refused_with_the_reason() {
        let (_scratch, project, dirs) = setup("invalid", true);
        let cases = [
            (input("Bad Name", &["read-only"], &["grep"]), "lowercase"),
            (
                input("a", &["root"], &["grep"]),
                "`root` is not a capability",
            ),
            (
                input("a", &["read-only", "write"], &["grep"]),
                "cannot be combined",
            ),
            (input("a", &[], &["grep"]), "at least one capability"),
            (input("a", &["write"], &[]), "at least one tool"),
        ];
        for (bad, reason) in cases {
            let error = save(&project, &dirs, bad, AgentScope::User, None).unwrap_err();
            assert!(error.contains(reason), "{error}");
        }
        let mut no_description = input("a", &["write"], &["edit"]);
        no_description.description = " ".into();
        let error = save(&project, &dirs, no_description, AgentScope::User, None).unwrap_err();
        assert!(error.contains("description"), "{error}");
    }

    #[test]
    fn a_broken_file_is_listed_and_can_be_repaired_as_text() {
        let (_scratch, project, dirs) = setup("repair", true);
        let dir = dirs.project.clone().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("broken.md"), "---\nname: broken\n").unwrap();
        let listed = list(&project, &dirs);
        assert!(listed.agents.is_empty());
        assert_eq!(listed.issues.len(), 1);
        assert_eq!(listed.issues[0].file_name, "broken.md");
        assert!(listed.issues[0].reason.contains("closing"));

        let broken = AgentRef {
            scope: AgentScope::Project,
            file_name: "broken.md".into(),
        };
        assert_eq!(read_source(&dirs, &broken).unwrap(), "---\nname: broken\n");
        let fixed = save_source(
            &project,
            &dirs,
            AgentScope::Project,
            Some(broken),
            "---\nname: broken\ndescription: Fixed.\nmode: [read-only]\ntools: [grep]\n---\nBody\n",
        )
        .unwrap();
        assert_eq!(fixed.description, "Fixed.");
        let listed = list(&project, &dirs);
        assert!(listed.issues.is_empty());
        assert_eq!(listed.agents.len(), 1);
    }

    #[test]
    fn the_tool_checklist_offers_builtins_with_their_class() {
        let tools = tools();
        let class = |name: &str| {
            tools
                .iter()
                .find(|tool| tool.name == name)
                .map(|tool| tool.class.as_str())
        };
        assert_eq!(class("read_file"), Some("read"));
        assert_eq!(class("edit"), Some("write"));
        assert_eq!(class("bash"), Some("exec"));
        assert_eq!(class("web_fetch"), Some("network"));
        assert_eq!(class("update_plan"), Some("meta"));
        for never in NEVER_DELEGATED_TOOLS {
            assert_eq!(class(never), None, "{never} is never offered");
        }
        assert!(tools.windows(2).all(|pair| pair[0].name < pair[1].name));
    }
}
