//! Custom agents: named, reusable child-agent definitions stored as markdown.
//!
//! Each agent is one file, `<name>.md`, with a YAML front matter block and a
//! markdown body:
//!
//! ```markdown
//! ---
//! name: code-reviewer
//! description: Reviews the current diff for correctness and missing tests.
//! mode:
//! - read-only
//! tools:
//! - read_file
//! - grep
//! ---
//! You are a careful code reviewer. …
//! ```
//!
//! The body is the agent's instructions: it becomes a role section in the
//! child's system prompt when the model starts the agent with `spawn_agent`.
//!
//! Two scopes are read:
//!
//! - **project** — `<project root>/.forge/agents/`. The project root is the
//!   main worktree of a Git project (never a session's linked worktree) or
//!   the folder itself, so every session of a project sees the same agents.
//! - **user** — `<user config dir>/forge/agents/`.
//!
//! A project agent wins over a user agent with the same name; the user one is
//! reported as overridden rather than silently dropped. Both directories sit
//! where the sandbox keeps agent-run commands from writing (`.forge` is
//! read-only to them, and Forge's own config directory is never writable), so
//! only Forge itself — through the desktop settings — writes these files.
//!
//! Loading never fails as a whole: a file that cannot be read or parsed is
//! skipped and reported as an [`AgentFileIssue`], and every other agent still
//! loads.

use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use forge_types::SideEffectClass;
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};

/// Largest agent file Forge reads. Agent instructions are prompt text; a file
/// past this is almost certainly not an agent definition.
pub const AGENT_FILE_MAX_BYTES: u64 = 256 * 1024;

/// Longest agent name. Names are also file names and worktree labels.
pub const AGENT_NAME_MAX_LEN: usize = 64;

/// Overrides the user-scope agents directory, to keep a demo or a second
/// setup apart from `<config dir>/forge/agents`.
pub const USER_AGENTS_DIR_ENV: &str = "FORGE_USER_AGENTS_DIR";

/// Tools a custom agent is never given, whatever its `tools` list says:
/// agents cannot start or coordinate further agents, or start detached
/// background jobs.
pub const NEVER_DELEGATED_TOOLS: &[&str] = &[
    "spawn_agent",
    "send_message",
    "wait_agent",
    "background_run",
];

/// Meta-class tools an agent may be given in any mode.
const ALWAYS_PERMITTED_META_TOOLS: &[&str] = &["ask_user_question", "update_plan"];

/// Runs a command outside the sandbox after human approval, which escapes both
/// the filesystem and the network confinement, so it needs both capabilities.
const UNCONFINED_RETRY_TOOL: &str = "request_unconfined_retry";

const FRONT_MATTER_DELIMITER: &str = "---";

/// Where an agent file lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentScope {
    Project,
    User,
}

impl AgentScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
        }
    }
}

impl fmt::Display for AgentScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One capability in an agent's `mode`. Reading is always allowed; the others
/// each unlock a class of tools. `read-only` means "reading only" and is
/// never combined with anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    ReadOnly,
    Write,
    Exec,
    Network,
}

impl Capability {
    pub const ALL: [Capability; 4] = [
        Capability::ReadOnly,
        Capability::Write,
        Capability::Exec,
        Capability::Network,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Write => "write",
            Self::Exec => "exec",
            Self::Network => "network",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|capability| capability.as_str() == value.trim())
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The content of one agent file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    /// Capabilities, sorted and without duplicates once normalized.
    pub mode: Vec<Capability>,
    /// Tool names in file order, without duplicates once normalized.
    pub tools: Vec<String>,
    /// The markdown body: the agent's role instructions.
    #[serde(default)]
    pub instructions: String,
}

impl AgentDefinition {
    pub fn has(&self, capability: Capability) -> bool {
        self.mode.contains(&capability)
    }

    /// Whether the agent needs its own worktree: anything that can change
    /// files (`write`, or `exec`, whose commands can write the workspace)
    /// must not share the parent's checkout.
    pub fn needs_worktree(&self) -> bool {
        self.has(Capability::Write) || self.has(Capability::Exec)
    }

    /// Whether the agent's sandboxed commands get the session's network
    /// gateway.
    pub fn network(&self) -> bool {
        self.has(Capability::Network)
    }

    /// `mode` as written in files and shown to the model: `read-only`,
    /// `write, exec`, …
    pub fn mode_label(&self) -> String {
        self.mode
            .iter()
            .map(|capability| capability.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Trim text fields, sort and dedupe `mode`, dedupe `tools`. Validation
    /// runs on the normalized form, and saved files are always normalized.
    pub fn normalized(&self) -> Self {
        let mode: BTreeSet<Capability> = self.mode.iter().copied().collect();
        let mut seen = HashSet::new();
        let tools = self
            .tools
            .iter()
            .map(|tool| tool.trim().to_string())
            .filter(|tool| !tool.is_empty() && seen.insert(tool.clone()))
            .collect();
        Self {
            name: self.name.trim().to_string(),
            description: self.description.trim().to_string(),
            mode: mode.into_iter().collect(),
            tools,
            instructions: normalize_body(&self.instructions),
        }
    }

    /// Check a normalized definition against every rule a file must meet.
    pub fn validate(&self) -> Result<(), String> {
        validate_name(&self.name)?;
        if self.description.trim().is_empty() {
            return Err("`description` must not be empty".into());
        }
        validate_mode(&self.mode)?;
        if self.tools.is_empty() {
            return Err("`tools` must list at least one tool".into());
        }
        Ok(())
    }

    pub fn file_name(&self) -> String {
        format!("{}.md", self.name)
    }
}

/// An agent that loaded, and where from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoadedAgent {
    #[serde(flatten)]
    pub definition: AgentDefinition,
    pub scope: AgentScope,
    pub path: PathBuf,
    pub file_name: String,
}

/// An agent file that was skipped, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentFileIssue {
    pub scope: AgentScope,
    pub path: PathBuf,
    pub file_name: String,
    pub reason: String,
}

impl AgentFileIssue {
    /// One line for a session notice.
    pub fn message(&self) -> String {
        format!(
            "Skipped {} agent file {}: {}",
            self.scope,
            self.path.display(),
            self.reason
        )
    }
}

/// Everything found in both scopes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AgentCatalog {
    /// The effective agents, sorted by name: every project agent, and every
    /// user agent no project agent overrides.
    pub agents: Vec<LoadedAgent>,
    /// User agents hidden by a project agent of the same name.
    pub overridden: Vec<LoadedAgent>,
    /// Files that were skipped.
    pub issues: Vec<AgentFileIssue>,
}

impl AgentCatalog {
    pub fn get(&self, name: &str) -> Option<&LoadedAgent> {
        self.agents
            .iter()
            .find(|agent| agent.definition.name == name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.agents
            .iter()
            .map(|agent| agent.definition.name.as_str())
            .collect()
    }

    /// Session notices for every skipped file.
    pub fn issue_messages(&self) -> Vec<String> {
        self.issues.iter().map(AgentFileIssue::message).collect()
    }
}

/// Identifies an existing agent file within a scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRef {
    pub scope: AgentScope,
    pub file_name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("project agents need an open project")]
    NoProject,
    #[error("the user config directory could not be resolved")]
    NoUserDirectory,
    #[error("{0}")]
    Invalid(String),
    #[error("a {scope} agent named `{name}` already exists")]
    AlreadyExists { scope: AgentScope, name: String },
    #[error("{0} was not found")]
    NotFound(PathBuf),
    #[error("`{0}` is not an agent file name")]
    BadFileName(String),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> AgentError + '_ {
    move |source| AgentError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// `<user config dir>/forge/agents`, or [`USER_AGENTS_DIR_ENV`] when set.
pub fn user_agents_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(USER_AGENTS_DIR_ENV) {
        return Some(PathBuf::from(dir));
    }
    dirs::config_dir().map(|dir| dir.join("forge").join("agents"))
}

/// `<project root>/.forge/agents`.
pub fn project_agents_dir(project_root: &Path) -> PathBuf {
    project_root.join(".forge").join("agents")
}

/// The two agent directories, resolved once. Every read and write goes
/// through this, so tests can point both scopes at temporary directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDirs {
    pub project: Option<PathBuf>,
    pub user: Option<PathBuf>,
}

impl AgentDirs {
    /// The directories for `project_root` (if any) and the current user.
    pub fn for_project(project_root: Option<&Path>) -> Self {
        Self {
            project: project_root.map(project_agents_dir),
            user: user_agents_dir(),
        }
    }

    pub fn dir(&self, scope: AgentScope) -> Result<&Path, AgentError> {
        match scope {
            AgentScope::Project => self.project.as_deref().ok_or(AgentError::NoProject),
            AgentScope::User => self.user.as_deref().ok_or(AgentError::NoUserDirectory),
        }
    }

    /// Read both scopes. Never fails: unreadable or invalid files become
    /// issues.
    pub fn discover(&self) -> AgentCatalog {
        let mut catalog = AgentCatalog::default();
        let project = self
            .project
            .as_deref()
            .map(|dir| read_scope(AgentScope::Project, dir, &mut catalog.issues))
            .unwrap_or_default();
        let user = self
            .user
            .as_deref()
            .map(|dir| read_scope(AgentScope::User, dir, &mut catalog.issues))
            .unwrap_or_default();
        let project_names: HashSet<String> = project
            .iter()
            .map(|agent| agent.definition.name.clone())
            .collect();
        catalog.agents = project;
        for agent in user {
            if project_names.contains(&agent.definition.name) {
                catalog.overridden.push(agent);
            } else {
                catalog.agents.push(agent);
            }
        }
        catalog
            .agents
            .sort_by(|a, b| a.definition.name.cmp(&b.definition.name));
        catalog
            .overridden
            .sort_by(|a, b| a.definition.name.cmp(&b.definition.name));
        catalog
            .issues
            .sort_by(|a, b| (a.scope, &a.file_name).cmp(&(b.scope, &b.file_name)));
        catalog
    }

    /// Create or update an agent. `previous` names the file being edited;
    /// when the name or scope changed, the agent moves and the old file is
    /// removed once the new one is safely written. Front-matter keys Forge
    /// does not know, found in the previous file, are kept.
    pub fn save(
        &self,
        definition: &AgentDefinition,
        scope: AgentScope,
        previous: Option<&AgentRef>,
    ) -> Result<LoadedAgent, AgentError> {
        let definition = definition.normalized();
        definition.validate().map_err(AgentError::Invalid)?;
        let extra = match previous {
            Some(previous) => {
                let path = self.existing_path(previous)?;
                read_agent_text(&path)
                    .ok()
                    .and_then(|text| parse_agent_file(&text, None).ok())
                    .map(|(_, extra)| extra)
                    .unwrap_or_default()
            }
            None => Mapping::new(),
        };
        self.write(
            &definition,
            scope,
            previous,
            &render_agent_file(&definition, &extra),
        )
    }

    /// Save hand-edited file text, used to repair a file that did not load.
    /// The text must parse as a valid agent; it is written under the name it
    /// declares, in `scope`, replacing `previous` if that is a different
    /// file.
    pub fn save_source(
        &self,
        scope: AgentScope,
        previous: Option<&AgentRef>,
        text: &str,
    ) -> Result<LoadedAgent, AgentError> {
        let (definition, extra) = parse_agent_file(text, None).map_err(AgentError::Invalid)?;
        let rendered = render_agent_file(&definition, &extra);
        self.write(&definition, scope, previous, &rendered)
    }

    /// The raw text of an agent file, valid or not.
    pub fn read_source(&self, agent: &AgentRef) -> Result<String, AgentError> {
        let path = self.existing_path(agent)?;
        read_agent_text(&path).map_err(|reason| AgentError::Invalid(reason.message()))
    }

    pub fn delete(&self, agent: &AgentRef) -> Result<(), AgentError> {
        let path = self.existing_path(agent)?;
        fs::remove_file(&path).map_err(io_error(&path))
    }

    fn existing_path(&self, agent: &AgentRef) -> Result<PathBuf, AgentError> {
        validate_file_name(&agent.file_name)?;
        let path = self.dir(agent.scope)?.join(&agent.file_name);
        match fs::symlink_metadata(&path) {
            Ok(_) => Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(AgentError::NotFound(path))
            }
            Err(error) => Err(AgentError::Io {
                path: path.clone(),
                source: error,
            }),
        }
    }

    fn write(
        &self,
        definition: &AgentDefinition,
        scope: AgentScope,
        previous: Option<&AgentRef>,
        text: &str,
    ) -> Result<LoadedAgent, AgentError> {
        let dir = self.dir(scope)?.to_path_buf();
        let file_name = definition.file_name();
        let target = dir.join(&file_name);
        let previous_path = previous
            .map(|previous| self.existing_path(previous))
            .transpose()?;
        let replaces_itself = previous
            .is_some_and(|previous| previous.scope == scope && previous.file_name == file_name);
        if !replaces_itself && fs::symlink_metadata(&target).is_ok() {
            return Err(AgentError::AlreadyExists {
                scope,
                name: definition.name.clone(),
            });
        }
        fs::create_dir_all(&dir).map_err(io_error(&dir))?;
        write_atomically(&target, text)?;
        if let Some(previous_path) = previous_path.filter(|_| !replaces_itself) {
            fs::remove_file(&previous_path).map_err(io_error(&previous_path))?;
        }
        Ok(LoadedAgent {
            definition: definition.clone(),
            scope,
            path: target,
            file_name,
        })
    }
}

/// Read every `*.md` file in one scope directory.
fn read_scope(scope: AgentScope, dir: &Path, issues: &mut Vec<AgentFileIssue>) -> Vec<LoadedAgent> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            issues.push(AgentFileIssue {
                scope,
                path: dir.to_path_buf(),
                file_name: String::new(),
                reason: format!("the agents directory could not be read: {error}"),
            });
            return Vec::new();
        }
    };
    let mut agents = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        // Hidden files include Forge's own in-progress temporary files.
        if file_name.starts_with('.') || !file_name.ends_with(".md") {
            continue;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let stem = file_name.trim_end_matches(".md");
        let parsed = read_agent_text(&path)
            .map_err(|reason| reason.message())
            .and_then(|text| parse_agent_file(&text, Some(stem)).map(|(definition, _)| definition));
        match parsed {
            Ok(definition) => agents.push(LoadedAgent {
                definition,
                scope,
                path,
                file_name,
            }),
            Err(reason) => issues.push(AgentFileIssue {
                scope,
                path,
                file_name,
                reason,
            }),
        }
    }
    agents
}

enum ReadFailure {
    TooLarge(u64),
    NotUtf8,
    Io(std::io::Error),
}

impl ReadFailure {
    fn message(&self) -> String {
        match self {
            Self::TooLarge(size) => format!(
                "the file is {size} bytes; agent files are limited to {AGENT_FILE_MAX_BYTES} bytes"
            ),
            Self::NotUtf8 => "the file is not valid UTF-8 text".into(),
            Self::Io(error) => format!("the file could not be read: {error}"),
        }
    }
}

fn read_agent_text(path: &Path) -> Result<String, ReadFailure> {
    let size = fs::metadata(path).map_err(ReadFailure::Io)?.len();
    if size > AGENT_FILE_MAX_BYTES {
        return Err(ReadFailure::TooLarge(size));
    }
    let bytes = fs::read(path).map_err(ReadFailure::Io)?;
    String::from_utf8(bytes).map_err(|_| ReadFailure::NotUtf8)
}

/// Parse and validate one agent file. `file_stem`, when given, is the file
/// name without `.md`, which the declared `name` must equal.
pub fn parse_agent(text: &str, file_stem: Option<&str>) -> Result<AgentDefinition, String> {
    parse_agent_file(text, file_stem).map(|(definition, _)| definition)
}

/// [`parse_agent`], also returning front-matter keys Forge does not use, so a
/// save can keep them.
fn parse_agent_file(
    text: &str,
    file_stem: Option<&str>,
) -> Result<(AgentDefinition, Mapping), String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let (front_matter, body) = split_front_matter(text)?;
    let value: Value = serde_yaml::from_str(&front_matter)
        .map_err(|error| format!("the front matter is not valid YAML: {error}"))?;
    let mut mapping = match value {
        Value::Mapping(mapping) => mapping,
        Value::Null => return Err("the front matter is empty".into()),
        _ => return Err("the front matter must be a set of `key: value` lines".into()),
    };
    let name = take_string(&mut mapping, "name")?;
    let description = take_string(&mut mapping, "description")?;
    let mode = take_list(&mut mapping, "mode")?
        .iter()
        .map(|value| {
            Capability::parse(value).ok_or_else(|| {
                format!(
                    "`mode` has the unknown capability `{value}` (use read-only, write, exec or network)"
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let tools = take_list(&mut mapping, "tools")?;
    let definition = AgentDefinition {
        name,
        description,
        mode,
        tools,
        instructions: body,
    }
    .normalized();
    definition.validate()?;
    if let Some(stem) = file_stem {
        if stem != definition.name {
            return Err(format!(
                "`name` is `{}` but the file is `{stem}.md`; they must match",
                definition.name
            ));
        }
    }
    Ok((definition, mapping))
}

/// Split `---\n<yaml>\n---\n<body>`. The opening delimiter must be the first
/// line.
fn split_front_matter(text: &str) -> Result<(String, String), String> {
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some(FRONT_MATTER_DELIMITER) {
        return Err("the file must start with a `---` line opening the front matter".into());
    }
    let rest: Vec<&str> = lines.collect();
    let end = rest
        .iter()
        .position(|line| line.trim_end() == FRONT_MATTER_DELIMITER)
        .ok_or("the front matter has no closing `---` line")?;
    Ok((rest[..end].join("\n"), rest[end + 1..].join("\n")))
}

fn take_string(mapping: &mut Mapping, key: &str) -> Result<String, String> {
    match mapping.remove(key) {
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(value),
        Some(Value::String(_)) | Some(Value::Null) | None => Err(format!("`{key}` is missing")),
        Some(_) => Err(format!("`{key}` must be text")),
    }
}

/// A list of strings. A single string is accepted as a one-item list, so a
/// hand-written `mode: read-only` works.
fn take_list(mapping: &mut Mapping, key: &str) -> Result<Vec<String>, String> {
    match mapping.remove(key) {
        None | Some(Value::Null) => Err(format!("`{key}` is missing")),
        Some(Value::String(value)) => Ok(vec![value]),
        Some(Value::Sequence(items)) => items
            .into_iter()
            .map(|item| match item {
                Value::String(value) => Ok(value),
                _ => Err(format!("every `{key}` entry must be text")),
            })
            .collect(),
        Some(_) => Err(format!("`{key}` must be a list")),
    }
}

/// `name` rules: 1–64 characters of lowercase letters, digits and single
/// hyphens, starting and ending with a letter or digit.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("`name` is missing".into());
    }
    if name.len() > AGENT_NAME_MAX_LEN {
        return Err(format!(
            "`name` is longer than {AGENT_NAME_MAX_LEN} characters"
        ));
    }
    let valid_chars = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !valid_chars || name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        return Err(format!(
            "`name` `{name}` must use lowercase letters, digits and single hyphens, \
             starting and ending with a letter or digit"
        ));
    }
    Ok(())
}

/// `mode` rules: at least one capability; `read-only` stands alone.
pub fn validate_mode(mode: &[Capability]) -> Result<(), String> {
    if mode.is_empty() {
        return Err("`mode` must list at least one capability".into());
    }
    if mode.contains(&Capability::ReadOnly) && mode.len() > 1 {
        return Err("`read-only` cannot be combined with other capabilities in `mode`".into());
    }
    Ok(())
}

/// An agent file name: one path component ending in `.md`, not hidden.
fn validate_file_name(file_name: &str) -> Result<(), AgentError> {
    let valid = file_name.ends_with(".md")
        && file_name.len() > ".md".len()
        && !file_name.starts_with('.')
        && !file_name.contains(['/', '\\', '\0']);
    if valid {
        Ok(())
    } else {
        Err(AgentError::BadFileName(file_name.to_string()))
    }
}

fn normalize_body(body: &str) -> String {
    body.trim_start_matches(['\n', '\r']).trim_end().to_string()
}

/// The file text for `definition`: known keys in a fixed order, then any
/// `extra` keys, then the instructions.
pub fn render_agent(definition: &AgentDefinition) -> String {
    render_agent_file(&definition.normalized(), &Mapping::new())
}

fn render_agent_file(definition: &AgentDefinition, extra: &Mapping) -> String {
    let mut mapping = Mapping::new();
    mapping.insert("name".into(), definition.name.clone().into());
    mapping.insert("description".into(), definition.description.clone().into());
    mapping.insert(
        "mode".into(),
        Value::Sequence(
            definition
                .mode
                .iter()
                .map(|capability| capability.as_str().into())
                .collect(),
        ),
    );
    mapping.insert(
        "tools".into(),
        Value::Sequence(
            definition
                .tools
                .iter()
                .map(|tool| tool.as_str().into())
                .collect(),
        ),
    );
    for (key, value) in extra {
        if !mapping.contains_key(key) {
            mapping.insert(key.clone(), value.clone());
        }
    }
    let yaml = serde_yaml::to_string(&mapping).unwrap_or_default();
    let mut text = format!("{FRONT_MATTER_DELIMITER}\n{yaml}{FRONT_MATTER_DELIMITER}\n");
    if !definition.instructions.is_empty() {
        text.push('\n');
        text.push_str(&definition.instructions);
        text.push('\n');
    }
    text
}

/// Write `text` to `target` via a temporary file and a rename, so a crash
/// mid-write never leaves a truncated agent file.
fn write_atomically(target: &Path, text: &str) -> Result<(), AgentError> {
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let file_name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or_default();
    let temporary = dir.join(format!(".{file_name}.tmp-{}-{nanos}", std::process::id()));
    let result = (|| {
        let mut out = fs::File::create(&temporary).map_err(io_error(&temporary))?;
        out.write_all(text.as_bytes())
            .map_err(io_error(&temporary))?;
        out.sync_all().map_err(io_error(&temporary))?;
        fs::rename(&temporary, target).map_err(io_error(target))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Whether `mode` lets an agent use `tool`, whose side-effect class is
/// `class`. Reading is always allowed; each other class needs its
/// capability; orchestration tools are never allowed.
pub fn capability_permits(mode: &[Capability], tool: &str, class: SideEffectClass) -> bool {
    if NEVER_DELEGATED_TOOLS.contains(&tool) {
        return false;
    }
    let has = |capability| mode.contains(&capability);
    if tool == UNCONFINED_RETRY_TOOL {
        return has(Capability::Exec) && has(Capability::Network);
    }
    match class {
        SideEffectClass::Read => true,
        SideEffectClass::Write => has(Capability::Write),
        SideEffectClass::Exec => has(Capability::Exec),
        SideEffectClass::Network => has(Capability::Network),
        SideEffectClass::Meta => ALWAYS_PERMITTED_META_TOOLS.contains(&tool),
        // A class added later is denied until a capability is defined for it.
        _ => false,
    }
}

/// The tools an agent actually gets, given the tools that exist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolResolution {
    /// Listed tools that exist and the mode permits, in file order.
    pub allowed: Vec<String>,
    /// One line per listed tool that was dropped.
    pub warnings: Vec<String>,
}

/// Resolve `definition.tools` against `available` (name, class) pairs.
pub fn resolve_agent_tools(
    definition: &AgentDefinition,
    available: &[(String, SideEffectClass)],
) -> ToolResolution {
    let mut resolution = ToolResolution::default();
    for tool in &definition.tools {
        let Some((_, class)) = available.iter().find(|(name, _)| name == tool) else {
            resolution.warnings.push(format!(
                "agent `{}`: tool `{tool}` is not available and was ignored",
                definition.name
            ));
            continue;
        };
        if capability_permits(&definition.mode, tool, *class) {
            resolution.allowed.push(tool.clone());
        } else if NEVER_DELEGATED_TOOLS.contains(&tool.as_str()) {
            resolution.warnings.push(format!(
                "agent `{}`: tool `{tool}` is never given to agents and was ignored",
                definition.name
            ));
        } else {
            resolution.warnings.push(format!(
                "agent `{}`: tool `{tool}` needs a capability not in mode `{}` and was ignored",
                definition.name,
                definition.mode_label()
            ));
        }
    }
    resolution
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn definition(name: &str) -> AgentDefinition {
        AgentDefinition {
            name: name.into(),
            description: "Reviews the current diff.".into(),
            mode: vec![Capability::ReadOnly],
            tools: vec!["read_file".into(), "grep".into()],
            instructions: "You are a careful reviewer.\n\nBe brief.".into(),
        }
    }

    struct Scopes {
        _root: TempDir,
        dirs: AgentDirs,
    }

    fn scopes() -> Scopes {
        let root = TempDir::new().unwrap();
        let dirs = AgentDirs {
            project: Some(root.path().join("project/.forge/agents")),
            user: Some(root.path().join("user/agents")),
        };
        Scopes { _root: root, dirs }
    }

    fn write(dir: &Path, file: &str, text: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(file);
        fs::write(&path, text).unwrap();
        path
    }

    const VALID: &str = "---\nname: code-reviewer\ndescription: Reviews diffs.\nmode:\n- read-only\ntools:\n- read_file\n- grep\n---\n\nYou review code.\n";

    #[test]
    fn a_valid_file_parses() {
        let agent = parse_agent(VALID, Some("code-reviewer")).unwrap();
        assert_eq!(agent.name, "code-reviewer");
        assert_eq!(agent.description, "Reviews diffs.");
        assert_eq!(agent.mode, vec![Capability::ReadOnly]);
        assert_eq!(agent.tools, vec!["read_file", "grep"]);
        assert_eq!(agent.instructions, "You review code.");
    }

    #[test]
    fn flow_lists_a_scalar_mode_crlf_and_a_bom_are_accepted() {
        let text = "\u{feff}---\r\nname: fixer\r\ndescription: Fixes tests.\r\nmode: write\r\ntools: [edit, read_file]\r\n---\r\nFix it.\r\n";
        let agent = parse_agent(text, Some("fixer")).unwrap();
        assert_eq!(agent.mode, vec![Capability::Write]);
        assert_eq!(agent.tools, vec!["edit", "read_file"]);
        assert_eq!(agent.instructions, "Fix it.");
    }

    #[test]
    fn rendering_round_trips_every_field() {
        let mut agent = definition("round-trip");
        agent.description = "Multi-line: with \"quotes\", colons: and #hashes\nsecond line".into();
        agent.mode = vec![Capability::Network, Capability::Exec, Capability::Write];
        agent.tools = vec!["bash".into(), "edit".into(), "web_fetch".into()];
        agent.instructions = "# Role\n\n- one\n- two\n\n---\n\nnot front matter".into();
        let text = render_agent(&agent);
        let parsed = parse_agent(&text, Some("round-trip")).unwrap();
        assert_eq!(parsed, agent.normalized());
        assert_eq!(
            parsed.mode,
            vec![Capability::Write, Capability::Exec, Capability::Network],
            "mode is saved sorted"
        );
        assert_eq!(render_agent(&parsed), text, "rendering is stable");
    }

    #[test]
    fn an_agent_with_no_instructions_round_trips() {
        let mut agent = definition("bare");
        agent.instructions.clear();
        let text = render_agent(&agent);
        assert!(text.ends_with("---\n"));
        assert_eq!(parse_agent(&text, Some("bare")).unwrap(), agent);
    }

    #[test]
    fn every_invalid_shape_is_rejected_with_a_reason() {
        let cases: &[(&str, &str)] = &[
            ("no front matter", "front matter"),
            ("---\nname: a\n", "closing `---`"),
            ("---\nname: [oops\n---\n", "not valid YAML"),
            ("---\n---\nbody", "empty"),
            ("---\n- a\n---\n", "`key: value`"),
            (
                "---\ndescription: d\nmode: [write]\ntools: [edit]\n---\n",
                "`name` is missing",
            ),
            (
                "---\nname: a\nmode: [write]\ntools: [edit]\n---\n",
                "`description` is missing",
            ),
            (
                "---\nname: a\ndescription: \"  \"\nmode: [write]\ntools: [edit]\n---\n",
                "`description` is missing",
            ),
            (
                "---\nname: a\ndescription: d\ntools: [edit]\n---\n",
                "`mode` is missing",
            ),
            (
                "---\nname: a\ndescription: d\nmode: []\ntools: [edit]\n---\n",
                "at least one capability",
            ),
            (
                "---\nname: a\ndescription: d\nmode: [admin]\ntools: [edit]\n---\n",
                "unknown capability `admin`",
            ),
            (
                "---\nname: a\ndescription: d\nmode: [read-only, write]\ntools: [edit]\n---\n",
                "cannot be combined",
            ),
            (
                "---\nname: a\ndescription: d\nmode: [write]\n---\n",
                "`tools` is missing",
            ),
            (
                "---\nname: a\ndescription: d\nmode: [write]\ntools: []\n---\n",
                "at least one tool",
            ),
            (
                "---\nname: a\ndescription: d\nmode: [write]\ntools: [1]\n---\n",
                "must be text",
            ),
            (
                "---\nname: a\ndescription: d\nmode: {x: 1}\ntools: [edit]\n---\n",
                "must be a list",
            ),
            (
                "---\nname: Bad Name\ndescription: d\nmode: [write]\ntools: [edit]\n---\n",
                "lowercase",
            ),
            (
                "---\nname: a--b\ndescription: d\nmode: [write]\ntools: [edit]\n---\n",
                "single hyphens",
            ),
            (
                "---\nname: 7\ndescription: d\nmode: [write]\ntools: [edit]\n---\n",
                "must be text",
            ),
        ];
        for (text, expected) in cases {
            let error = parse_agent(text, None).unwrap_err();
            assert!(error.contains(expected), "{text:?}: {error}");
        }
        let long = "a".repeat(AGENT_NAME_MAX_LEN + 1);
        assert!(validate_name(&long).unwrap_err().contains("longer than"));
        assert!(validate_name(&"a".repeat(AGENT_NAME_MAX_LEN)).is_ok());
    }

    #[test]
    fn the_name_must_match_the_file_name() {
        let error = parse_agent(VALID, Some("other")).unwrap_err();
        assert!(error.contains("must match"), "{error}");
    }

    #[test]
    fn discovery_loads_both_scopes_and_the_project_overrides_the_user() {
        let s = scopes();
        let project = s.dirs.project.clone().unwrap();
        let user = s.dirs.user.clone().unwrap();
        write(&project, "shared.md", &render_agent(&definition("shared")));
        write(
            &project,
            "only-project.md",
            &render_agent(&definition("only-project")),
        );
        write(&user, "shared.md", &render_agent(&definition("shared")));
        write(
            &user,
            "only-user.md",
            &render_agent(&definition("only-user")),
        );

        let catalog = s.dirs.discover();
        let effective: Vec<_> = catalog
            .agents
            .iter()
            .map(|agent| (agent.definition.name.as_str(), agent.scope))
            .collect();
        assert_eq!(
            effective,
            vec![
                ("only-project", AgentScope::Project),
                ("only-user", AgentScope::User),
                ("shared", AgentScope::Project),
            ]
        );
        assert_eq!(catalog.overridden.len(), 1);
        assert_eq!(catalog.overridden[0].definition.name, "shared");
        assert_eq!(catalog.overridden[0].scope, AgentScope::User);
        assert!(catalog.issues.is_empty());
    }

    #[test]
    fn a_bad_file_is_reported_and_the_rest_still_load() {
        let s = scopes();
        let project = s.dirs.project.clone().unwrap();
        write(&project, "good.md", &render_agent(&definition("good")));
        write(&project, "broken.md", "---\nname: broken\n");
        write(
            &project,
            "renamed.md",
            &render_agent(&definition("other-name")),
        );
        fs::write(project.join("binary.md"), [0xff, 0xfe, 0x00]).unwrap();
        fs::write(
            project.join("huge.md"),
            "x".repeat(AGENT_FILE_MAX_BYTES as usize + 1),
        )
        .unwrap();
        write(&project, "notes.txt", "not an agent");
        write(&project, ".good.md.tmp-1-2", "half written");
        fs::create_dir_all(project.join("folder.md")).unwrap();

        let catalog = s.dirs.discover();
        assert_eq!(catalog.names(), vec!["good"]);
        let issues: Vec<_> = catalog
            .issues
            .iter()
            .map(|issue| (issue.file_name.as_str(), issue.reason.as_str()))
            .collect();
        assert_eq!(issues.len(), 4, "{issues:?}");
        assert!(issues[0].0 == "binary.md" && issues[0].1.contains("UTF-8"));
        assert!(issues[1].0 == "broken.md" && issues[1].1.contains("closing"));
        assert!(issues[2].0 == "huge.md" && issues[2].1.contains("limited"));
        assert!(issues[3].0 == "renamed.md" && issues[3].1.contains("must match"));
        let message = catalog.issue_messages()[1].clone();
        assert!(
            message.starts_with("Skipped project agent file "),
            "{message}"
        );
        assert!(message.contains("broken.md"), "{message}");
    }

    #[test]
    fn missing_directories_are_simply_empty() {
        let s = scopes();
        assert_eq!(s.dirs.discover(), AgentCatalog::default());
        let none = AgentDirs {
            project: None,
            user: None,
        };
        assert_eq!(none.discover(), AgentCatalog::default());
    }

    #[test]
    fn save_creates_loads_and_refuses_to_overwrite_another_agent() {
        let s = scopes();
        let saved = s
            .dirs
            .save(&definition("reviewer"), AgentScope::Project, None)
            .unwrap();
        assert_eq!(saved.file_name, "reviewer.md");
        assert!(saved.path.ends_with("project/.forge/agents/reviewer.md"));
        let catalog = s.dirs.discover();
        assert_eq!(
            catalog.get("reviewer").unwrap().definition,
            definition("reviewer").normalized()
        );

        let error = s
            .dirs
            .save(&definition("reviewer"), AgentScope::Project, None)
            .unwrap_err();
        assert!(matches!(error, AgentError::AlreadyExists { .. }), "{error}");
        // The same name in the other scope is allowed: that is an override.
        s.dirs
            .save(&definition("reviewer"), AgentScope::User, None)
            .unwrap();
    }

    #[test]
    fn save_validates_before_writing_anything() {
        let s = scopes();
        let mut bad = definition("reviewer");
        bad.tools.clear();
        let error = s.dirs.save(&bad, AgentScope::User, None).unwrap_err();
        assert!(error.to_string().contains("at least one tool"), "{error}");
        assert!(!s.dirs.user.clone().unwrap().exists());
    }

    #[test]
    fn editing_changes_every_field_in_place() {
        let s = scopes();
        s.dirs
            .save(&definition("reviewer"), AgentScope::User, None)
            .unwrap();
        let edited = AgentDefinition {
            name: "reviewer".into(),
            description: "Now reviews and fixes.".into(),
            mode: vec![Capability::Write, Capability::Exec],
            tools: vec!["edit".into(), "bash".into()],
            instructions: "New instructions.".into(),
        };
        let previous = AgentRef {
            scope: AgentScope::User,
            file_name: "reviewer.md".into(),
        };
        s.dirs
            .save(&edited, AgentScope::User, Some(&previous))
            .unwrap();
        let catalog = s.dirs.discover();
        assert_eq!(catalog.agents.len(), 1);
        assert_eq!(catalog.agents[0].definition, edited.normalized());
    }

    #[test]
    fn renaming_and_moving_scope_moves_the_file() {
        let s = scopes();
        s.dirs
            .save(&definition("reviewer"), AgentScope::User, None)
            .unwrap();
        let previous = AgentRef {
            scope: AgentScope::User,
            file_name: "reviewer.md".into(),
        };
        let renamed = s
            .dirs
            .save(&definition("auditor"), AgentScope::Project, Some(&previous))
            .unwrap();
        assert_eq!(renamed.scope, AgentScope::Project);
        let catalog = s.dirs.discover();
        assert_eq!(catalog.names(), vec!["auditor"]);
        assert!(!s.dirs.user.clone().unwrap().join("reviewer.md").exists());

        // Renaming onto an existing agent is refused and leaves both intact.
        s.dirs
            .save(&definition("other"), AgentScope::Project, None)
            .unwrap();
        let previous = AgentRef {
            scope: AgentScope::Project,
            file_name: "auditor.md".into(),
        };
        let error = s
            .dirs
            .save(&definition("other"), AgentScope::Project, Some(&previous))
            .unwrap_err();
        assert!(matches!(error, AgentError::AlreadyExists { .. }));
        assert_eq!(s.dirs.discover().names(), vec!["auditor", "other"]);
    }

    #[test]
    fn unknown_front_matter_keys_survive_an_edit() {
        let s = scopes();
        let user = s.dirs.user.clone().unwrap();
        write(
            &user,
            "reviewer.md",
            "---\nname: reviewer\ndescription: d\nmode: [read-only]\ntools: [grep]\ncolor: teal\nx-team: [a, b]\n---\nBody\n",
        );
        let previous = AgentRef {
            scope: AgentScope::User,
            file_name: "reviewer.md".into(),
        };
        s.dirs
            .save(&definition("reviewer"), AgentScope::User, Some(&previous))
            .unwrap();
        let text = fs::read_to_string(user.join("reviewer.md")).unwrap();
        assert!(text.contains("color: teal"), "{text}");
        assert!(text.contains("x-team:"), "{text}");
        assert!(text.contains("You are a careful reviewer."), "{text}");
    }

    #[test]
    fn a_broken_file_can_be_read_fixed_as_text_and_deleted() {
        let s = scopes();
        let project = s.dirs.project.clone().unwrap();
        write(&project, "Broken Name.md", "---\nname: oops\n");
        let broken = AgentRef {
            scope: AgentScope::Project,
            file_name: "Broken Name.md".into(),
        };
        assert_eq!(s.dirs.read_source(&broken).unwrap(), "---\nname: oops\n");

        let error = s
            .dirs
            .save_source(AgentScope::Project, Some(&broken), "---\nname: oops\n")
            .unwrap_err();
        assert!(matches!(error, AgentError::Invalid(_)));
        assert!(
            project.join("Broken Name.md").exists(),
            "an invalid fix writes nothing"
        );

        let fixed = s
            .dirs
            .save_source(AgentScope::Project, Some(&broken), VALID)
            .unwrap();
        assert_eq!(fixed.file_name, "code-reviewer.md");
        assert!(!project.join("Broken Name.md").exists());
        let catalog = s.dirs.discover();
        assert_eq!(catalog.names(), vec!["code-reviewer"]);
        assert!(catalog.issues.is_empty());

        s.dirs
            .delete(&AgentRef {
                scope: AgentScope::Project,
                file_name: "code-reviewer.md".into(),
            })
            .unwrap();
        assert!(s.dirs.discover().agents.is_empty());
    }

    #[test]
    fn file_references_cannot_escape_the_scope_directory() {
        let s = scopes();
        for bad in [
            "../secret.md",
            "a/b.md",
            ".hidden.md",
            "notes.txt",
            ".md",
            "a\\b.md",
        ] {
            let error = s
                .dirs
                .delete(&AgentRef {
                    scope: AgentScope::User,
                    file_name: bad.into(),
                })
                .unwrap_err();
            assert!(
                matches!(error, AgentError::BadFileName(_)),
                "{bad}: {error}"
            );
        }
        let error = s
            .dirs
            .delete(&AgentRef {
                scope: AgentScope::User,
                file_name: "absent.md".into(),
            })
            .unwrap_err();
        assert!(matches!(error, AgentError::NotFound(_)));
    }

    #[test]
    fn project_scope_needs_a_project() {
        let dirs = AgentDirs {
            project: None,
            user: None,
        };
        let error = dirs
            .save(&definition("reviewer"), AgentScope::Project, None)
            .unwrap_err();
        assert!(matches!(error, AgentError::NoProject));
    }

    #[test]
    fn capabilities_gate_tool_classes() {
        use SideEffectClass::*;
        let read_only = [Capability::ReadOnly];
        assert!(capability_permits(&read_only, "read_file", Read));
        assert!(capability_permits(&read_only, "update_plan", Meta));
        assert!(capability_permits(&read_only, "ask_user_question", Meta));
        assert!(!capability_permits(&read_only, "edit", Write));
        assert!(!capability_permits(&read_only, "bash", Exec));
        assert!(!capability_permits(&read_only, "web_fetch", Network));

        let write_exec = [Capability::Write, Capability::Exec];
        assert!(capability_permits(&write_exec, "edit", Write));
        assert!(capability_permits(&write_exec, "bash", Exec));
        assert!(!capability_permits(&write_exec, "web_fetch", Network));
        assert!(!capability_permits(
            &write_exec,
            "request_unconfined_retry",
            Meta
        ));

        let everything = [Capability::Write, Capability::Exec, Capability::Network];
        assert!(capability_permits(
            &everything,
            "request_unconfined_retry",
            Meta
        ));
        assert!(!capability_permits(&everything, "some_meta_tool", Meta));
        for tool in NEVER_DELEGATED_TOOLS {
            assert!(!capability_permits(&everything, tool, Exec), "{tool}");
            assert!(!capability_permits(&everything, tool, Meta), "{tool}");
        }
    }

    #[test]
    fn tool_resolution_keeps_permitted_tools_and_explains_the_rest() {
        use SideEffectClass::*;
        let available: Vec<(String, SideEffectClass)> = [
            ("read_file", Read),
            ("edit", Write),
            ("bash", Exec),
            ("spawn_agent", Meta),
        ]
        .into_iter()
        .map(|(name, class)| (name.to_string(), class))
        .collect();
        let mut agent = definition("mixed");
        agent.tools = vec![
            "read_file".into(),
            "edit".into(),
            "bash".into(),
            "spawn_agent".into(),
            "mcp:github:search".into(),
        ];
        agent.mode = vec![Capability::Write];
        let resolution = resolve_agent_tools(&agent, &available);
        assert_eq!(resolution.allowed, vec!["read_file", "edit"]);
        assert_eq!(resolution.warnings.len(), 3);
        assert!(resolution.warnings[0].contains("`bash` needs a capability not in mode `write`"));
        assert!(resolution.warnings[1].contains("`spawn_agent` is never given"));
        assert!(resolution.warnings[2].contains("`mcp:github:search` is not available"));
    }

    #[test]
    fn scheduling_follows_the_capabilities() {
        let mut agent = definition("a");
        assert!(!agent.needs_worktree() && !agent.network());
        agent.mode = vec![Capability::Network];
        assert!(!agent.needs_worktree() && agent.network());
        agent.mode = vec![Capability::Exec];
        assert!(agent.needs_worktree() && !agent.network());
        agent.mode = vec![Capability::Write, Capability::Network];
        assert!(agent.needs_worktree() && agent.network());
        assert_eq!(agent.mode_label(), "write, network");
    }
}
