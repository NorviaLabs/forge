//! Workspace-confined filesystem operations for the File Explorer.

use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOperationKind {
    CreateFile,
    CreateDirectory,
    RenameEntry,
    DeleteEntry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMode {
    Trash,
    Permanent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOperationResult {
    pub kind: FileOperationKind,
    pub path: PathBuf,
    pub new_path: Option<PathBuf>,
    pub parent: PathBuf,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FileOperationError {
    #[error("Name cannot be empty.")]
    EmptyName,
    #[error("Name must be one file or folder name, not a path.")]
    PathName,
    #[error("Name `.` and `..` are not allowed.")]
    DotName,
    #[error("Name is not valid on this platform.")]
    InvalidName,
    #[error("Path is outside the workspace.")]
    OutsideWorkspace,
    #[error("Parent folder no longer exists.")]
    MissingParent,
    #[error("Selected entry no longer exists.")]
    MissingSource,
    #[error("Destination already exists.")]
    AlreadyExists,
    #[error("Permission denied.")]
    PermissionDenied,
    #[error("Filesystem is read-only.")]
    ReadOnly,
    #[error("Symlink safety could not be established.")]
    SymlinkAmbiguous,
    #[error("Trash is unavailable: {0}")]
    TrashUnavailable(String),
    #[error("File operation failed: {0}")]
    Io(String),
}

impl FileOperationError {
    pub fn actionable(&self) -> String {
        match self {
            Self::TrashUnavailable(reason) => format!(
                "Trash is unavailable: {reason}\nUse permanent delete only if you are sure."
            ),
            Self::PermissionDenied => "Permission denied. Check file or folder permissions.".into(),
            Self::ReadOnly => "The filesystem is read-only.".into(),
            _ => self.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceFileOps {
    root: PathBuf,
}

impl WorkspaceFileOps {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, FileOperationError> {
        let root = root.as_ref().canonicalize().map_err(map_io_error)?;
        Ok(Self { root })
    }

    pub fn validate_name(&self, name: &str) -> Result<OsString, FileOperationError> {
        validate_entry_name(name)
    }

    pub fn plan_create(&self, parent: &Path, name: &str) -> Result<PathBuf, FileOperationError> {
        let name = self.validate_name(name)?;
        let parent = self.resolve_existing_directory(parent)?;
        let dest = self.child_path(&parent, &name)?;
        if lexists(&dest) {
            return Err(FileOperationError::AlreadyExists);
        }
        Ok(dest)
    }

    pub fn plan_rename(
        &self,
        source: &Path,
        new_name: &str,
    ) -> Result<PathBuf, FileOperationError> {
        let new_name = self.validate_name(new_name)?;
        let source = self.resolve_existing_entry_no_follow(source)?;
        let parent = source
            .parent()
            .ok_or(FileOperationError::OutsideWorkspace)
            .and_then(|p| self.resolve_existing_directory(p))?;
        let dest = self.child_path(&parent, &new_name)?;
        let case_only = same_path_case_insensitive(&source, &dest) && source != dest;
        if source != dest && !case_only && lexists(&dest) {
            return Err(FileOperationError::AlreadyExists);
        }
        Ok(dest)
    }

    pub fn create_file(
        &self,
        parent: &Path,
        name: &str,
    ) -> Result<FileOperationResult, FileOperationError> {
        let name = self.validate_name(name)?;
        let parent = self.resolve_existing_directory(parent)?;
        let dest = self.child_path(&parent, &name)?;
        if lexists(&dest) {
            return Err(FileOperationError::AlreadyExists);
        }
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
            .map_err(map_io_error)?;
        Ok(FileOperationResult {
            kind: FileOperationKind::CreateFile,
            path: dest,
            new_path: None,
            parent,
        })
    }

    pub fn create_directory(
        &self,
        parent: &Path,
        name: &str,
    ) -> Result<FileOperationResult, FileOperationError> {
        let name = self.validate_name(name)?;
        let parent = self.resolve_existing_directory(parent)?;
        let dest = self.child_path(&parent, &name)?;
        if lexists(&dest) {
            return Err(FileOperationError::AlreadyExists);
        }
        fs::create_dir(&dest).map_err(map_io_error)?;
        Ok(FileOperationResult {
            kind: FileOperationKind::CreateDirectory,
            path: dest,
            new_path: None,
            parent,
        })
    }

    pub fn rename_entry(
        &self,
        source: &Path,
        new_name: &str,
    ) -> Result<FileOperationResult, FileOperationError> {
        let new_name = self.validate_name(new_name)?;
        let source = self.resolve_existing_entry_no_follow(source)?;
        let parent = source
            .parent()
            .ok_or(FileOperationError::OutsideWorkspace)
            .and_then(|p| self.resolve_existing_directory(p))?;
        let dest = self.child_path(&parent, &new_name)?;
        if source == dest {
            return Ok(FileOperationResult {
                kind: FileOperationKind::RenameEntry,
                path: source.clone(),
                new_path: Some(dest),
                parent,
            });
        }
        let case_only = same_path_case_insensitive(&source, &dest) && source != dest;
        if !case_only && lexists(&dest) {
            return Err(FileOperationError::AlreadyExists);
        }
        if case_only {
            self.case_only_rename(&source, &dest)?;
        } else {
            fs::rename(&source, &dest).map_err(map_io_error)?;
        }
        Ok(FileOperationResult {
            kind: FileOperationKind::RenameEntry,
            path: source,
            new_path: Some(dest),
            parent,
        })
    }

    pub fn delete_entry(
        &self,
        source: &Path,
        mode: DeleteMode,
    ) -> Result<FileOperationResult, FileOperationError> {
        let source = self.resolve_existing_entry_no_follow(source)?;
        let parent = source
            .parent()
            .ok_or(FileOperationError::OutsideWorkspace)?
            .to_path_buf();
        match mode {
            DeleteMode::Trash => move_to_trash(&source)?,
            DeleteMode::Permanent => delete_permanently_no_follow(&source)?,
        }
        Ok(FileOperationResult {
            kind: FileOperationKind::DeleteEntry,
            path: source,
            new_path: None,
            parent,
        })
    }

    pub fn entry_kind(&self, source: &Path) -> Result<EntryKind, FileOperationError> {
        let source = self.resolve_existing_entry_no_follow(source)?;
        let ft = fs::symlink_metadata(source)
            .map_err(map_io_error)?
            .file_type();
        Ok(if ft.is_symlink() {
            EntryKind::Symlink
        } else if ft.is_dir() {
            EntryKind::Directory
        } else if ft.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        })
    }

    pub fn is_non_empty_directory(&self, source: &Path) -> Result<bool, FileOperationError> {
        let source = self.resolve_existing_entry_no_follow(source)?;
        let meta = fs::symlink_metadata(&source).map_err(map_io_error)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Ok(false);
        }
        Ok(fs::read_dir(source).map_err(map_io_error)?.next().is_some())
    }

    fn resolve_existing_directory(&self, path: &Path) -> Result<PathBuf, FileOperationError> {
        let resolved = path.canonicalize().map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                FileOperationError::MissingParent
            } else {
                map_io_error(error)
            }
        })?;
        if !resolved.is_dir() {
            return Err(FileOperationError::MissingParent);
        }
        self.ensure_inside(&resolved)?;
        Ok(resolved)
    }

    fn resolve_existing_entry_no_follow(&self, path: &Path) -> Result<PathBuf, FileOperationError> {
        let meta = fs::symlink_metadata(path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                FileOperationError::MissingSource
            } else {
                map_io_error(error)
            }
        })?;
        if meta.file_type().is_symlink() {
            let parent = path.parent().ok_or(FileOperationError::OutsideWorkspace)?;
            let parent = self.resolve_existing_directory(parent)?;
            let full = parent.join(
                path.file_name()
                    .ok_or(FileOperationError::OutsideWorkspace)?,
            );
            self.ensure_lexical_child(&parent, &full)?;
            return Ok(full);
        }
        let resolved = path.canonicalize().map_err(map_io_error)?;
        self.ensure_inside(&resolved)?;
        Ok(resolved)
    }

    fn child_path(&self, parent: &Path, name: &OsStr) -> Result<PathBuf, FileOperationError> {
        self.ensure_inside(parent)?;
        let child = parent.join(name);
        self.ensure_lexical_child(parent, &child)?;
        Ok(child)
    }

    fn ensure_inside(&self, path: &Path) -> Result<(), FileOperationError> {
        if path == self.root || path.starts_with(&self.root) {
            Ok(())
        } else {
            Err(FileOperationError::OutsideWorkspace)
        }
    }

    fn ensure_lexical_child(&self, parent: &Path, child: &Path) -> Result<(), FileOperationError> {
        let child_parent = child.parent().ok_or(FileOperationError::OutsideWorkspace)?;
        if child_parent == parent && child.starts_with(&self.root) {
            Ok(())
        } else {
            Err(FileOperationError::OutsideWorkspace)
        }
    }

    fn case_only_rename(&self, source: &Path, dest: &Path) -> Result<(), FileOperationError> {
        let parent = source
            .parent()
            .ok_or(FileOperationError::OutsideWorkspace)?;
        let temporary = unique_temp_name(parent);
        if lexists(&temporary) {
            return Err(FileOperationError::AlreadyExists);
        }
        fs::rename(source, &temporary).map_err(map_io_error)?;
        if let Err(error) = fs::rename(&temporary, dest) {
            let _ = fs::rename(&temporary, source);
            return Err(map_io_error(error));
        }
        Ok(())
    }
}

fn validate_entry_name(name: &str) -> Result<OsString, FileOperationError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(FileOperationError::EmptyName);
    }
    let path = Path::new(trimmed);
    if path.is_absolute() || path.components().count() != 1 {
        return Err(FileOperationError::PathName);
    }
    if matches!(
        path.components().next(),
        Some(Component::ParentDir | Component::CurDir)
    ) {
        return Err(FileOperationError::DotName);
    }
    if trimmed == "." || trimmed == ".." {
        return Err(FileOperationError::DotName);
    }
    if trimmed.contains(std::path::MAIN_SEPARATOR)
        || trimmed.contains('/')
        || trimmed.contains('\\')
    {
        return Err(FileOperationError::PathName);
    }
    if trimmed.contains('\0') {
        return Err(FileOperationError::InvalidName);
    }
    #[cfg(windows)]
    {
        let lower = trimmed.trim_end_matches([' ', '.']).to_ascii_lowercase();
        if lower.is_empty()
            || matches!(
                lower.as_str(),
                "con"
                    | "prn"
                    | "aux"
                    | "nul"
                    | "com1"
                    | "com2"
                    | "com3"
                    | "com4"
                    | "com5"
                    | "com6"
                    | "com7"
                    | "com8"
                    | "com9"
                    | "lpt1"
                    | "lpt2"
                    | "lpt3"
                    | "lpt4"
                    | "lpt5"
                    | "lpt6"
                    | "lpt7"
                    | "lpt8"
                    | "lpt9"
            )
            || trimmed.contains(['<', '>', ':', '"', '|', '?', '*'])
        {
            return Err(FileOperationError::InvalidName);
        }
    }
    Ok(OsString::from(trimmed))
}

fn delete_permanently_no_follow(path: &Path) -> Result<(), FileOperationError> {
    let meta = fs::symlink_metadata(path).map_err(map_io_error)?;
    if meta.file_type().is_symlink() || meta.is_file() {
        fs::remove_file(path).map_err(map_io_error)
    } else if meta.is_dir() {
        fs::remove_dir_all(path).map_err(map_io_error)
    } else {
        Err(FileOperationError::SymlinkAmbiguous)
    }
}

fn move_to_trash(path: &Path) -> Result<(), FileOperationError> {
    let trash_dir = platform_trash_dir()?;
    fs::create_dir_all(&trash_dir).map_err(|error| {
        FileOperationError::TrashUnavailable(format!("could not create Trash: {error}"))
    })?;
    let dest = unique_trash_path(
        &trash_dir,
        path.file_name().unwrap_or_else(|| OsStr::new("entry")),
    );
    fs::rename(path, dest).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            FileOperationError::MissingSource
        } else {
            FileOperationError::TrashUnavailable(error.to_string())
        }
    })
}

fn platform_trash_dir() -> Result<PathBuf, FileOperationError> {
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().ok_or_else(|| {
            FileOperationError::TrashUnavailable("home directory not found".into())
        })?;
        Ok(home.join(".Trash"))
    }
    #[cfg(target_os = "linux")]
    {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
            .ok_or_else(|| {
                FileOperationError::TrashUnavailable("home directory not found".into())
            })?;
        Ok(base.join("Trash/files"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err(FileOperationError::TrashUnavailable(
            "Trash is not implemented on this platform".into(),
        ))
    }
}

fn unique_trash_path(dir: &Path, name: &OsStr) -> PathBuf {
    let mut candidate = dir.join(name);
    if !lexists(&candidate) {
        return candidate;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    for i in 0..1000 {
        let mut next = OsString::from(name);
        next.push(format!(".forge-trash-{stamp}-{i}"));
        candidate = dir.join(next);
        if !lexists(&candidate) {
            return candidate;
        }
    }
    dir.join(format!("forge-trash-{stamp}"))
}

fn unique_temp_name(parent: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    parent.join(format!(".forge-rename-{stamp}"))
}

fn lexists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn same_path_case_insensitive(left: &Path, right: &Path) -> bool {
    left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase()
}

fn map_io_error(error: io::Error) -> FileOperationError {
    match error.kind() {
        io::ErrorKind::PermissionDenied => FileOperationError::PermissionDenied,
        io::ErrorKind::AlreadyExists => FileOperationError::AlreadyExists,
        io::ErrorKind::NotFound => FileOperationError::MissingSource,
        io::ErrorKind::ReadOnlyFilesystem => FileOperationError::ReadOnly,
        _ => FileOperationError::Io(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_single_names() {
        let root = tempfile::tempdir().unwrap();
        let ops = WorkspaceFileOps::new(root.path()).unwrap();
        assert!(ops.validate_name("file.rs").is_ok());
        assert!(ops.validate_name(".env").is_ok());
        assert!(ops.validate_name("雪.rs").is_ok());
        assert_eq!(
            ops.validate_name("   ").unwrap_err(),
            FileOperationError::EmptyName
        );
        assert_eq!(
            ops.validate_name(".").unwrap_err(),
            FileOperationError::DotName
        );
        assert_eq!(
            ops.validate_name("..").unwrap_err(),
            FileOperationError::DotName
        );
        assert_eq!(
            ops.validate_name("src/new.rs").unwrap_err(),
            FileOperationError::PathName
        );
        assert_eq!(
            ops.validate_name("/tmp/new.rs").unwrap_err(),
            FileOperationError::PathName
        );
    }

    #[test]
    fn create_file_and_folder_reject_collisions() {
        let root = tempfile::tempdir().unwrap();
        let ops = WorkspaceFileOps::new(root.path()).unwrap();
        ops.create_file(root.path(), "a.txt").unwrap();
        ops.create_directory(root.path(), "src").unwrap();
        assert!(root.path().join("a.txt").is_file());
        assert!(root.path().join("src").is_dir());
        assert_eq!(
            ops.create_file(root.path(), "a.txt").unwrap_err(),
            FileOperationError::AlreadyExists
        );
    }

    #[test]
    fn rename_stays_in_parent_and_handles_case_only() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("makefile");
        fs::write(&path, "").unwrap();
        let ops = WorkspaceFileOps::new(root.path()).unwrap();
        let renamed = ops.rename_entry(&path, "Makefile").unwrap();
        assert_eq!(
            renamed.new_path.unwrap(),
            root.path().join("Makefile").canonicalize().unwrap()
        );
        assert!(root.path().join("Makefile").exists());
        assert_eq!(
            ops.rename_entry(&root.path().join("Makefile"), "src/makefile")
                .unwrap_err(),
            FileOperationError::PathName
        );
    }

    #[test]
    fn symlink_delete_does_not_follow_target() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("target.txt");
        fs::write(&target, "keep").unwrap();
        let link = root.path().join("link.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &link).unwrap();

        let ops = WorkspaceFileOps::new(root.path()).unwrap();
        ops.delete_entry(&link, DeleteMode::Permanent).unwrap();
        assert!(!link.exists());
        assert!(target.exists());
    }

    #[test]
    fn rejects_parent_outside_workspace() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let ops = WorkspaceFileOps::new(root.path()).unwrap();
        assert_eq!(
            ops.create_file(outside.path(), "x").unwrap_err(),
            FileOperationError::OutsideWorkspace
        );
    }
}
