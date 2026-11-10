//! The registry of projects the desktop window can open: `projects.toml` in
//! the user config directory.
//!
//! A project is a root folder: the main worktree of a Git repository, or a
//! plain folder. Roots never overlap. The registry holds no runtime state;
//! whether a project is open, trusted or owned is decided elsewhere.
//!
//! Writes take `projects.lock`, re-read the file, validate, and replace the
//! file with a rename, so concurrent writers never lose an entry and a reader
//! never sees half a file. Reads take no lock.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ProjectTopology;

const FORMAT_VERSION: u32 = 1;

/// A project's identity. It lives only in `projects.toml`; nothing is
/// written into the project folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(uuid::Uuid);

impl ProjectId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for ProjectId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ProjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for ProjectId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        uuid::Uuid::parse_str(value).map(Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    /// Canonical. For a Git repository, the main worktree.
    pub root: PathBuf,
    /// The folder name, for display. Not unique; the UI disambiguates.
    pub name: String,
    pub added_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("no user config directory to keep the project registry in")]
    NoConfigDir,
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not valid: {detail}")]
    Invalid { path: PathBuf, detail: String },
    /// Written by a newer Forge. Never overwritten.
    #[error("{path} has format version {version}; this Forge reads version {FORMAT_VERSION}")]
    UnknownVersion { path: PathBuf, version: u32 },
    #[error("{0} is not a folder")]
    NotAFolder(PathBuf),
    #[error("{root} overlaps the project `{}` at {}", existing.name, existing.root.display())]
    Overlaps { root: PathBuf, existing: Project },
    #[error("no project with id {0}")]
    NotFound(ProjectId),
    #[error(transparent)]
    Topology(#[from] crate::TopologyError),
}

/// What a launch folder resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchResolution {
    Project(Project),
    /// A plain folder no project contains. Registering it lets agents edit
    /// everything inside it, so the caller must ask first and then call
    /// [`ProjectRegistry::add`].
    NeedsConfirmation {
        root: PathBuf,
    },
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    version: u32,
    #[serde(default, rename = "project")]
    projects: Vec<Project>,
}

#[derive(Debug, Clone)]
pub struct ProjectRegistry {
    path: PathBuf,
}

impl ProjectRegistry {
    /// The user's registry: `<config dir>/forge/projects.toml`.
    pub fn user() -> Result<Self, RegistryError> {
        dirs::config_dir()
            .map(|dir| Self::at(dir.join("forge").join("projects.toml")))
            .ok_or(RegistryError::NoConfigDir)
    }

    /// A registry at `path`. Tests use a temporary file.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn list(&self) -> Result<Vec<Project>, RegistryError> {
        Ok(self.read()?.projects)
    }

    pub fn get(&self, id: ProjectId) -> Result<Project, RegistryError> {
        self.list()?
            .into_iter()
            .find(|project| project.id == id)
            .ok_or(RegistryError::NotFound(id))
    }

    /// Register the project containing `path`: the repository's main
    /// worktree, or the folder itself. Registering a root that is already
    /// registered returns the existing project.
    pub fn add(&self, path: &Path) -> Result<Project, RegistryError> {
        let root = project_root(path)?;
        self.update(|file| {
            if let Some(existing) = file.projects.iter().find(|p| p.root == root) {
                return Ok(existing.clone());
            }
            check_overlap(&file.projects, &root, None)?;
            let project = Project {
                id: ProjectId::new(),
                name: folder_name(&root),
                root,
                added_at: Utc::now(),
            };
            file.projects.push(project.clone());
            Ok(project)
        })
    }

    /// Unregister a project. Never touches its folder or saved sessions.
    pub fn remove(&self, id: ProjectId) -> Result<Project, RegistryError> {
        self.update(|file| {
            let index = file
                .projects
                .iter()
                .position(|project| project.id == id)
                .ok_or(RegistryError::NotFound(id))?;
            Ok(file.projects.remove(index))
        })
    }

    /// Point a project whose folder moved at its new location, keeping its id.
    pub fn relocate(&self, id: ProjectId, path: &Path) -> Result<Project, RegistryError> {
        let root = project_root(path)?;
        self.update(|file| {
            check_overlap(&file.projects, &root, Some(id))?;
            if let Some(other) = file.projects.iter().find(|p| p.root == root && p.id != id) {
                return Err(RegistryError::Overlaps {
                    root: root.clone(),
                    existing: other.clone(),
                });
            }
            let project = file
                .projects
                .iter_mut()
                .find(|project| project.id == id)
                .ok_or(RegistryError::NotFound(id))?;
            project.name = folder_name(&root);
            project.root = root;
            Ok(project.clone())
        })
    }

    /// The project a launch from `cwd` should open. A registered project
    /// containing `cwd` wins; otherwise a Git repository is registered
    /// without asking; otherwise the caller must confirm the folder.
    pub fn resolve_launch(&self, cwd: &Path) -> Result<LaunchResolution, RegistryError> {
        let cwd = canonical_folder(cwd)?;
        if let Some(project) = self
            .list()?
            .into_iter()
            .find(|project| cwd.starts_with(&project.root))
        {
            return Ok(LaunchResolution::Project(project));
        }
        let topology = ProjectTopology::detect(&cwd)?;
        if topology.supports_git() {
            return self.add(&cwd).map(LaunchResolution::Project);
        }
        Ok(LaunchResolution::NeedsConfirmation { root: cwd })
    }

    fn read(&self) -> Result<RegistryFile, RegistryError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RegistryFile {
                    version: FORMAT_VERSION,
                    projects: Vec::new(),
                })
            }
            Err(source) => return Err(self.io(source)),
        };
        // Check the version before the rest, so a newer layout is reported
        // as newer rather than as invalid.
        #[derive(Deserialize)]
        struct Version {
            version: u32,
        }
        let invalid = |error: toml::de::Error| RegistryError::Invalid {
            path: self.path.clone(),
            detail: error.to_string(),
        };
        let Version { version } = toml::from_str(&text).map_err(invalid)?;
        if version != FORMAT_VERSION {
            return Err(RegistryError::UnknownVersion {
                path: self.path.clone(),
                version,
            });
        }
        toml::from_str(&text).map_err(invalid)
    }

    /// Read, change and replace the file under `projects.lock`.
    fn update<T>(
        &self,
        change: impl FnOnce(&mut RegistryFile) -> Result<T, RegistryError>,
    ) -> Result<T, RegistryError> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(dir).map_err(|source| self.io(source))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("projects.lock"))
            .map_err(|source| self.io(source))?;
        lock.lock().map_err(|source| self.io(source))?;
        let mut file = self.read()?;
        let result = change(&mut file)?;
        self.write(&file)?;
        drop(lock);
        Ok(result)
    }

    fn write(&self, file: &RegistryFile) -> Result<(), RegistryError> {
        let text = toml::to_string(file).map_err(|error| RegistryError::Invalid {
            path: self.path.clone(),
            detail: error.to_string(),
        })?;
        let temporary = self
            .path
            .with_extension(format!("toml.tmp-{}", std::process::id()));
        let mut out = File::create(&temporary).map_err(|source| self.io(source))?;
        out.write_all(text.as_bytes())
            .and_then(|()| out.sync_all())
            .map_err(|source| self.io(source))?;
        fs::rename(&temporary, &self.path).map_err(|source| self.io(source))
    }

    fn io(&self, source: std::io::Error) -> RegistryError {
        RegistryError::Io {
            path: self.path.clone(),
            source,
        }
    }
}

fn canonical_folder(path: &Path) -> Result<PathBuf, RegistryError> {
    let canonical = path.canonicalize().map_err(|source| RegistryError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !canonical.is_dir() {
        return Err(RegistryError::NotAFolder(canonical));
    }
    Ok(canonical)
}

/// The canonical project root for `path`.
fn project_root(path: &Path) -> Result<PathBuf, RegistryError> {
    let folder = canonical_folder(path)?;
    canonical_folder(ProjectTopology::detect(&folder)?.root())
}

/// Roots may not contain one another. `ignore` is the project being moved.
fn check_overlap(
    projects: &[Project],
    root: &Path,
    ignore: Option<ProjectId>,
) -> Result<(), RegistryError> {
    match projects.iter().find(|project| {
        Some(project.id) != ignore
            && project.root != root
            && (root.starts_with(&project.root) || project.root.starts_with(root))
    }) {
        Some(existing) => Err(RegistryError::Overlaps {
            root: root.to_path_buf(),
            existing: existing.clone(),
        }),
        None => Ok(()),
    }
}

fn folder_name(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("project")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_test_support::init_repo;
    use tempfile::TempDir;

    struct Fixture {
        _config: TempDir,
        work: TempDir,
        registry: ProjectRegistry,
    }

    fn fixture() -> Fixture {
        let config = TempDir::new().unwrap();
        let registry = ProjectRegistry::at(config.path().join("forge").join("projects.toml"));
        Fixture {
            _config: config,
            work: TempDir::new().unwrap(),
            registry,
        }
    }

    fn folder(fixture: &Fixture, name: &str) -> PathBuf {
        let path = fixture.work.path().join(name);
        fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }

    #[test]
    fn a_missing_file_is_an_empty_registry_and_add_survives_a_reload() {
        let f = fixture();
        assert!(f.registry.list().unwrap().is_empty());
        let notes = folder(&f, "notes");
        let added = f.registry.add(&notes).unwrap();
        assert_eq!((added.root.clone(), added.name.as_str()), (notes, "notes"));

        let reloaded = ProjectRegistry::at(f.registry.path());
        assert_eq!(reloaded.list().unwrap(), vec![added.clone()]);
        assert_eq!(reloaded.get(added.id).unwrap(), added);
        let text = fs::read_to_string(f.registry.path()).unwrap();
        assert!(text.starts_with("version = 1"), "{text}");
        assert!(text.contains("[[project]]"), "{text}");
    }

    #[test]
    fn adding_the_same_root_twice_returns_the_existing_project() {
        let f = fixture();
        let notes = folder(&f, "notes");
        let first = f.registry.add(&notes).unwrap();
        assert_eq!(f.registry.add(&notes).unwrap(), first);
        assert_eq!(f.registry.list().unwrap().len(), 1);
    }

    #[test]
    fn a_repository_subfolder_registers_the_main_worktree() {
        let f = fixture();
        let repo = folder(&f, "api");
        init_repo(&repo);
        let src = repo.join("src");
        fs::create_dir_all(&src).unwrap();
        assert_eq!(f.registry.add(&src).unwrap().root, repo);
    }

    #[test]
    fn nested_roots_are_refused_both_ways() {
        let f = fixture();
        let outer = folder(&f, "outer");
        let inner = folder(&f, "outer/inner");
        f.registry.add(&inner).unwrap();
        assert!(matches!(
            f.registry.add(&outer),
            Err(RegistryError::Overlaps { existing, .. }) if existing.root == inner
        ));

        let g = fixture();
        let outer = folder(&g, "outer");
        let inner = folder(&g, "outer/inner");
        g.registry.add(&outer).unwrap();
        assert!(matches!(
            g.registry.add(&inner),
            Err(RegistryError::Overlaps { .. })
        ));
    }

    #[test]
    fn remove_and_relocate_keep_the_id_and_never_touch_the_folder() {
        let f = fixture();
        let old = folder(&f, "old");
        let project = f.registry.add(&old).unwrap();
        let moved = folder(&f, "moved");
        let relocated = f.registry.relocate(project.id, &moved).unwrap();
        assert_eq!(relocated.id, project.id);
        assert_eq!(
            (relocated.root, relocated.name.as_str()),
            (moved.clone(), "moved")
        );

        assert_eq!(f.registry.remove(project.id).unwrap().id, project.id);
        assert!(f.registry.list().unwrap().is_empty());
        assert!(moved.is_dir());
        assert!(matches!(
            f.registry.remove(project.id),
            Err(RegistryError::NotFound(_))
        ));
    }

    #[test]
    fn launch_resolution_prefers_a_registered_project_then_git_then_asks() {
        let f = fixture();
        let notes = folder(&f, "notes");
        let deep = folder(&f, "notes/a/b");
        assert_eq!(
            f.registry.resolve_launch(&notes).unwrap(),
            LaunchResolution::NeedsConfirmation {
                root: notes.clone()
            }
        );
        assert!(
            f.registry.list().unwrap().is_empty(),
            "asking writes nothing"
        );

        let project = f.registry.add(&notes).unwrap();
        assert_eq!(
            f.registry.resolve_launch(&deep).unwrap(),
            LaunchResolution::Project(project)
        );

        let repo = folder(&f, "api");
        init_repo(&repo);
        let LaunchResolution::Project(api) = f.registry.resolve_launch(&repo).unwrap() else {
            panic!("a repository registers without asking");
        };
        assert_eq!(api.root, repo);
        assert_eq!(f.registry.list().unwrap().len(), 2);
    }

    #[test]
    fn a_newer_format_is_reported_and_never_overwritten() {
        let f = fixture();
        fs::create_dir_all(f.registry.path().parent().unwrap()).unwrap();
        let newer = "version = 2\nfuture = true\n";
        fs::write(f.registry.path(), newer).unwrap();
        assert!(matches!(
            f.registry.list(),
            Err(RegistryError::UnknownVersion { version: 2, .. })
        ));
        let notes = folder(&f, "notes");
        assert!(f.registry.add(&notes).is_err());
        assert_eq!(fs::read_to_string(f.registry.path()).unwrap(), newer);
    }

    #[test]
    fn concurrent_adds_keep_every_project() {
        let f = fixture();
        let roots: Vec<_> = (0..8).map(|i| folder(&f, &format!("p{i}"))).collect();
        std::thread::scope(|scope| {
            for root in &roots {
                let registry = f.registry.clone();
                scope.spawn(move || registry.add(root).unwrap());
            }
        });
        assert_eq!(f.registry.list().unwrap().len(), 8);
    }
}
