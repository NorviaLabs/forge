//! Workspace browsing, editing and git for the desktop frontend: a file tree,
//! file contents, a guarded write, file create/rename/delete, content search,
//! and the git verbs an operator runs by hand (stage, unstage, commit,
//! branches).
//!
//! Every path is confined to the active session's workspace.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::Serialize;

/// A workspace larger than this (or one that nests deeper) is truncated
/// rather than allowed to stall the webview.
const MAX_ENTRIES: usize = 4000;
const MAX_DEPTH: usize = 12;
/// The editor cuts a file here; larger files open read-only.
const MAX_FILE_BYTES: usize = 512 * 1024;
const MAX_DIFF_BYTES: usize = 512 * 1024;

/// Directories that would flood the tree or expose runtime data.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".forge",
    ".impeccable",
    "node_modules",
    "target",
    "dist",
    "dist-self-test",
    "__pycache__",
    ".venv",
    "venv",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileEntry {
    /// Workspace-relative path with `/` separators.
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    /// Nesting level from the workspace root, for the flat tree rendering.
    pub depth: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileContents {
    pub path: String,
    /// LF-normalized text (`write` restores `crlf`).
    pub text: String,
    pub truncated: bool,
    /// SHA-256 of the bytes read. `write` refuses a buffer that no longer
    /// matches, so an agent edit cannot be clobbered by a stale editor.
    pub fingerprint: String,
    /// Why this file may not be edited, if it may not.
    pub read_only: Option<String>,
    /// The file used CRLF line endings; saves put them back.
    pub crlf: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedFile {
    pub path: String,
    /// One letter: M, A, D, ?, U.
    pub status: String,
    /// The index has a change for this path (safe to commit).
    pub staged: bool,
    /// The working tree has a change (safe to stage).
    pub unstaged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceStatus {
    pub branch: String,
    pub files: Vec<ChangedFile>,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub merge_in_progress: bool,
    pub conflicts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileDiff {
    pub path: String,
    pub text: String,
    pub truncated: bool,
    pub side: DiffSide,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffSide {
    #[default]
    Combined,
    Staged,
    Unstaged,
    LastTurn,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GitOperation {
    Fetch,
    Pull,
    Push,
    AbortMerge,
    DeleteBranch { name: String, force: bool },
    CommitAll { message: String },
}

fn local_git(root: &Path) -> Result<forge_workspace::git_service::LocalGit, String> {
    forge_workspace::git_service::LocalGit::new(root).map_err(|error| error.to_string())
}

pub fn git_operation(root: &Path, operation: GitOperation) -> Result<String, String> {
    let service = local_git(root)?;
    let result = match operation {
        GitOperation::Fetch => service.fetch(),
        GitOperation::Pull => service.pull(),
        GitOperation::Push => service.push(),
        GitOperation::AbortMerge => service.abort_merge(),
        GitOperation::DeleteBranch { name, force } => service.delete_branch(&name, force),
        GitOperation::CommitAll { message } => {
            if message.trim().is_empty() || message.contains('\0') {
                return Err("A nonempty commit message without NUL is required".into());
            }
            service.commit_all(&message)
        }
    };
    result.map_err(|error| error.to_string())
}

/// Resolve a workspace-relative path, refusing anything that escapes it.
pub fn resolve(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("path leaves the workspace".into());
    }
    let path = root.join(relative);
    let resolved = path
        .canonicalize()
        .map_err(|_| "no such file in the workspace".to_string())?;
    let root = root
        .canonicalize()
        .map_err(|_| "the workspace is gone".to_string())?;
    if !resolved.starts_with(&root) {
        return Err("path leaves the workspace".into());
    }
    Ok(resolved)
}

/// Resolve an existing entry without following a final symlink, so file
/// operations act on the link itself rather than its target.
fn resolve_entry(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        || relative.file_name().is_none()
    {
        return Err("path leaves the workspace".into());
    }
    let root = root
        .canonicalize()
        .map_err(|_| "the workspace is gone".to_string())?;
    let parent_rel = relative.parent().unwrap_or(Path::new(""));
    let parent = if parent_rel.as_os_str().is_empty() {
        root.clone()
    } else {
        let parent = root
            .join(parent_rel)
            .canonicalize()
            .map_err(|_| "no such file in the workspace".to_string())?;
        if !parent.starts_with(&root) {
            return Err("path leaves the workspace".into());
        }
        parent
    };
    let path = parent.join(relative.file_name().expect("file name checked above"));
    if path.symlink_metadata().is_err() {
        return Err("no such file in the workspace".into());
    }
    Ok(path)
}

/// Resolve a path that does not exist yet: the parent must be inside the
/// workspace, and the name may not escape it.
fn resolve_new(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        || relative.file_name().is_none()
    {
        return Err("path leaves the workspace".into());
    }
    let root = root
        .canonicalize()
        .map_err(|_| "the workspace is gone".to_string())?;
    // The deepest part that exists is canonicalized; the rest is a name we
    // are about to create, and is safe because no component is `..`.
    let mut existing = PathBuf::new();
    for component in relative.components() {
        let candidate = existing.join(component);
        if root.join(&candidate).symlink_metadata().is_err() {
            break;
        }
        existing = candidate;
    }
    let base = if existing.as_os_str().is_empty() {
        root.clone()
    } else {
        resolve(&root, &path_to_slash(&existing))?
    };
    let rest = relative.strip_prefix(&existing).unwrap_or(relative);
    let path = base.join(rest);
    if !path.starts_with(&root) {
        return Err("path leaves the workspace".into());
    }
    Ok(path)
}

/// Depth-first, directories before files, skipping heavy and runtime dirs.
pub fn list(root: &Path) -> Result<Vec<FileEntry>, String> {
    let root = root
        .canonicalize()
        .map_err(|_| "the workspace is gone".to_string())?;
    let mut entries = Vec::new();
    walk(&root, &root, 0, &mut entries);
    Ok(entries)
}

fn walk(root: &Path, dir: &Path, depth: usize, entries: &mut Vec<FileEntry>) {
    if depth >= MAX_DEPTH || entries.len() >= MAX_ENTRIES {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<(bool, String, PathBuf)> = read
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let is_dir = entry.file_type().ok()?.is_dir();
            if is_dir && SKIP_DIRS.contains(&name.as_str()) {
                return None;
            }
            Some((is_dir, name, path))
        })
        .collect();
    // Directories first, then names, so the flat tree reads like a tree.
    children.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    for (is_dir, name, path) in children {
        if entries.len() >= MAX_ENTRIES {
            return;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        entries.push(FileEntry {
            path: path_to_slash(relative),
            name,
            is_dir,
            depth,
        });
        if is_dir {
            walk(root, &path, depth + 1, entries);
        }
    }
}

fn path_to_slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Read a UTF-8 text file, capped. Binary files are refused, not mangled.
/// Anything the editor could not save back byte-for-byte is marked
/// `read_only` instead of being silently rewritten.
pub fn read(root: &Path, relative: &str) -> Result<FileContents, String> {
    let path = resolve(root, relative)?;
    let metadata = std::fs::metadata(&path).map_err(|_| "cannot read that file".to_string())?;
    if !metadata.is_file() {
        return Err("not a file".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .map_err(|_| "cannot read that file".to_string())?
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read that file".to_string())?;
    if bytes.iter().take(8192).any(|byte| *byte == 0) {
        return Err("binary file; not shown in the viewer".into());
    }
    let truncated = bytes.len() > MAX_FILE_BYTES;
    let fingerprint = sha256(&bytes);
    if truncated {
        bytes.truncate(MAX_FILE_BYTES);
    }
    let crlf = bytes.windows(2).any(|window| window == b"\r\n");
    let (text, read_only) = match std::str::from_utf8(&bytes) {
        Ok(text) => (text.replace("\r\n", "\n"), truncated_message(truncated)),
        Err(_) => (
            String::from_utf8_lossy(&bytes).into_owned(),
            Some("this file is not valid UTF-8; it is shown read-only".to_string()),
        ),
    };
    Ok(FileContents {
        path: relative.to_string(),
        text,
        truncated,
        fingerprint,
        read_only,
        crlf,
    })
}

fn truncated_message(truncated: bool) -> Option<String> {
    truncated.then(|| "this file is larger than 512 KB; it is shown read-only".to_string())
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Replace an existing text file. The write goes through a sibling temp file
/// so an interrupted save leaves the original intact, and is refused if the
/// file changed since the editor read it.
pub fn write(
    root: &Path,
    relative: &str,
    text: &str,
    fingerprint: &str,
    crlf: bool,
) -> Result<FileContents, String> {
    if text.len() > MAX_FILE_BYTES * 2 {
        return Err("file is too large to save from the desktop editor".into());
    }
    let path = resolve(root, relative)?;
    if !path.is_file() {
        return Err("only existing files can be saved".into());
    }
    let current = std::fs::read(&path).map_err(|_| "cannot read that file".to_string())?;
    if current.len() > MAX_FILE_BYTES {
        return Err("the file grew past the editor's limit; reopen it before saving".into());
    }
    if sha256(&current) != fingerprint {
        return Err("the file changed on disk; reopen it before saving".into());
    }
    let normalized = text.replace("\r\n", "\n");
    let bytes = if crlf {
        normalized.replace('\n', "\r\n")
    } else {
        normalized.clone()
    };
    let parent = path
        .parent()
        .ok_or_else(|| "the file has no parent directory".to_string())?;
    // Never reuse a predictable temp path: it could already be a symlink to a
    // file outside the workspace. `create_new` makes the name ours or fails.
    let mut temp = None;
    for attempt in 0..64u32 {
        let candidate = parent.join(format!(".forge-save-{}-{attempt}", std::process::id()));
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("cannot save: {error}")),
        };
        use std::io::Write;
        if let Err(error) = file.write_all(bytes.as_bytes()) {
            let _ = std::fs::remove_file(&candidate);
            return Err(format!("cannot save: {error}"));
        }
        temp = Some(candidate);
        break;
    }
    let temp = temp.ok_or_else(|| "cannot save: no free temporary name".to_string())?;
    std::fs::rename(&temp, &path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("cannot save: {error}")
    })?;
    Ok(FileContents {
        path: relative.to_string(),
        text: normalized,
        truncated: false,
        fingerprint: sha256(bytes.as_bytes()),
        read_only: None,
        crlf,
    })
}

/// Discard the working-tree change to one tracked file.
pub fn restore(root: &Path, relative: &str) -> Result<(), String> {
    forge_workspace::git_review::restore_path(root, Path::new(relative))
}

/// `git status` for the workspace, plus the current branch.
pub fn status(root: &Path) -> Result<WorkspaceStatus, String> {
    let service = local_git(root)?;
    let branch = service.branch_status().map_err(|error| error.to_string())?;
    let conflicts = service
        .conflict_state()
        .map_err(|error| error.to_string())?;
    let raw = git(root, &["status", "--porcelain=v1", "-z"])?;
    Ok(WorkspaceStatus {
        branch: branch.branch.unwrap_or_default(),
        files: parse_status(&raw),
        upstream: branch.upstream,
        ahead: branch.ahead,
        behind: branch.behind,
        merge_in_progress: conflicts.merge_in_progress,
        conflicts: conflicts
            .paths
            .iter()
            .map(|path| path_to_slash(path))
            .collect(),
    })
}

/// Parse porcelain v1 `-z` output. Renames carry the original path in the
/// next NUL-terminated field, which is skipped.
fn parse_status(raw: &str) -> Vec<ChangedFile> {
    let mut files = Vec::new();
    let mut fields = raw.split('\0');
    while let Some(field) = fields.next() {
        if field.len() < 4 {
            continue;
        }
        let mut chars = field.chars();
        let (Some(staged), Some(unstaged)) = (chars.next(), chars.next()) else {
            continue;
        };
        // Skip the space after "XY"; paths may begin with anything else.
        let path = field[3..].to_string();
        if staged == 'R' || staged == 'C' {
            // The source path follows; not part of the change we show.
            fields.next();
        }
        let status = if staged == 'U' || unstaged == 'U' {
            'U'
        } else if staged == '?' || unstaged == '?' {
            '?'
        } else if staged != ' ' {
            staged
        } else {
            unstaged
        };
        if status == '!' {
            continue;
        }
        files.push(ChangedFile {
            path,
            status: status.to_string(),
            staged: staged != ' ' && staged != '?' && staged != '!',
            // Untracked files have no index side at all.
            unstaged: (unstaged != ' ' && unstaged != '!') || staged == '?',
        });
    }
    files
}

/// The worktree diff for one path. Untracked files are shown as additions.
pub fn diff(root: &Path, relative: &str, side: DiffSide) -> Result<FileDiff, String> {
    let path = Path::new(relative);
    let mut text = match side {
        DiffSide::Combined => forge_workspace::git_review::combined_diff_text(root, path)?.0,
        DiffSide::Staged => forge_workspace::git_review::staged_diff_text(root, path)?,
        DiffSide::Unstaged => {
            forge_workspace::git_review::validated_relative_path(root, path)?;
            git(root, &["diff", "--no-color", "--", relative])?
        }
        // The last turn's patch comes from its write-tool cards, not from git.
        DiffSide::LastTurn => return Err("last-turn diffs are read from the transcript".into()),
    };
    let truncated = text.len() > MAX_DIFF_BYTES;
    if truncated {
        let mut end = MAX_DIFF_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Ok(FileDiff {
        path: relative.to_string(),
        text,
        truncated,
        side,
    })
}

/// A file pointed at from the open editor, sent with the next prompt.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PromptAttachment {
    pub path: String,
    /// 0-based line in the editor buffer.
    pub line: usize,
}

/// The context block the TUI also prepends for an attached file.
pub fn attachment_text(root: &Path, attachment: &PromptAttachment) -> Result<String, String> {
    let path = resolve(root, &attachment.path)?;
    if !path.is_file() {
        return Err("only files can be attached to a prompt".into());
    }
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > 1024 * 1024) {
        return Err("that file is too large to attach; open a smaller one".into());
    }
    forge_workspace::file_context::build_attachment_text(
        &path,
        attachment.line,
        &attachment.path,
        150,
    )
    .map_err(|error| error.to_string())
}

/// Reverse-apply one working-tree hunk. Staged and combined review both use
/// the same HEAD-to-worktree hunk list, so the index matches either view.
pub fn discard_hunk(root: &Path, relative: &str, hunk_index: usize) -> Result<(), String> {
    forge_workspace::git_review::discard_hunk(root, Path::new(relative), hunk_index)
}

/// An image the webview can display: magic-byte checked, size capped, and
/// inlined as a data URL because the page cannot read the filesystem.
#[derive(Debug, Clone, Serialize)]
pub struct ImageContents {
    pub path: String,
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub byte_len: u64,
    pub data_url: String,
}

pub fn read_image(root: &Path, relative: &str) -> Result<ImageContents, String> {
    use base64::Engine;
    let path = resolve(root, relative)?;
    let byte_len = std::fs::metadata(&path)
        .map_err(|_| "no such file in the workspace".to_string())?
        .len();
    if byte_len > forge_types::MAX_IMAGE_BYTES {
        return Err("image is too large to preview (limit 5 MiB)".into());
    }
    let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
    let meta = forge_types::inspect_image(&bytes).map_err(|error| error.to_string())?;
    Ok(ImageContents {
        path: relative.into(),
        mime: meta.mime.into(),
        width: meta.width,
        height: meta.height,
        byte_len,
        data_url: format!(
            "data:{};base64,{}",
            meta.mime,
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        ),
    })
}

/// Store pasted images in the workspace and return their references.
pub fn store_images(root: &Path, images: &[String]) -> Result<Vec<forge_types::ImageRef>, String> {
    use base64::Engine;
    use std::io::Write;
    if images.is_empty() || images.len() > 4 {
        return Err("Attach between one and four images".into());
    }
    let decoded = images
        .iter()
        .map(|encoded| {
            if encoded.len() > (forge_types::MAX_IMAGE_BYTES as usize).div_ceil(3) * 4 {
                return Err("Image exceeds the 5 MiB limit".into());
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| "Invalid image encoding")?;
            let meta = forge_types::inspect_image(&bytes).map_err(|error| error.to_string())?;
            Ok((bytes, meta))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let dir = resolve_new(root, ".forge/attachments")?;
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("Cannot create attachment storage: {error}"))?;
    let dir = resolve(root, ".forge/attachments")?;
    let mut created = Vec::new();
    let result = decoded
        .into_iter()
        .map(|(bytes, meta)| {
            let name = format!(
                "{}.{}",
                forge_types::SessionId::new_v4(),
                match meta.mime {
                    "image/png" => "png",
                    "image/jpeg" => "jpg",
                    "image/gif" => "gif",
                    _ => "webp",
                }
            );
            let path = dir.join(&name);
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|error| error.to_string())?;
            created.push(path);
            file.write_all(&bytes).map_err(|error| error.to_string())?;
            Ok(forge_types::ImageRef::new(
                format!(".forge/attachments/{name}"),
                meta.mime,
                bytes.len() as u64,
            )
            .with_dimensions(meta.width, meta.height))
        })
        .collect::<Result<Vec<_>, String>>();
    if result.is_err() {
        for path in created {
            let _ = std::fs::remove_file(path);
        }
    }
    result
}

/// Create an empty file. Refused if anything already exists at that path.
pub fn create_file(root: &Path, relative: &str) -> Result<(), String> {
    let path = resolve_new(root, relative)?;
    if path.exists() {
        return Err("that path already exists".into());
    }
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map(|_| ())
        .map_err(|error| format!("cannot create the file: {error}"))
}

/// Create a directory (and missing parents inside the workspace).
pub fn create_dir(root: &Path, relative: &str) -> Result<(), String> {
    let path = resolve_new(root, relative)?;
    if path.exists() {
        return Err("that path already exists".into());
    }
    std::fs::create_dir_all(&path).map_err(|error| format!("cannot create the folder: {error}"))
}

/// Rename or move one path inside the workspace. Refused across filesystems,
/// and refused when the destination exists.
pub fn rename(root: &Path, from: &str, to: &str) -> Result<(), String> {
    let source = resolve_entry(root, from)?;
    let destination = resolve_new(root, to)?;
    if destination.exists() {
        return Err("something already exists at the new path".into());
    }
    std::fs::rename(&source, &destination).map_err(|error| format!("cannot rename: {error}"))
}

/// Delete a file, or an empty directory. A non-empty directory is refused so
/// one click cannot erase a tree.
pub fn delete(root: &Path, relative: &str) -> Result<(), String> {
    let path = resolve_entry(root, relative)?;
    if path.symlink_metadata().is_ok_and(|meta| meta.is_dir()) {
        std::fs::remove_dir(&path)
            .map_err(|_| "the folder is not empty; delete its files first".to_string())
    } else {
        std::fs::remove_file(&path).map_err(|error| format!("cannot delete: {error}"))
    }
}

/// One content-search hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchHit {
    pub path: String,
    /// 1-based line number.
    pub line: usize,
    pub text: String,
}

/// Files larger than this are not searched.
const MAX_SEARCH_FILE_BYTES: u64 = 1024 * 1024;
const MAX_SEARCH_HITS: usize = 200;
/// A search reads at most this many files; a huge tree truncates silently.
const MAX_SEARCH_FILES: usize = 2000;

/// Case-insensitive substring search across the workspace's text files.
pub fn search(root: &Path, query: &str) -> Result<Vec<SearchHit>, String> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut hits = Vec::new();
    let mut read = 0;
    for entry in list(root)?.into_iter().filter(|entry| !entry.is_dir) {
        if hits.len() >= MAX_SEARCH_HITS || read >= MAX_SEARCH_FILES {
            break;
        }
        let path = resolve(root, &entry.path)?;
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.len() > MAX_SEARCH_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if bytes.iter().take(8192).any(|byte| *byte == 0) {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        read += 1;
        for (index, line) in text.lines().enumerate() {
            if hits.len() >= MAX_SEARCH_HITS {
                break;
            }
            if !line.to_lowercase().contains(&query) {
                continue;
            }
            let shown: String = line.chars().take(200).collect();
            hits.push(SearchHit {
                path: entry.path.clone(),
                line: index + 1,
                text: shown,
            });
        }
    }
    Ok(hits)
}

/// Stage one path.
pub fn stage(root: &Path, relative: &str) -> Result<(), String> {
    local_git(root)?
        .stage(Path::new(relative))
        .map_err(|error| error.to_string())
}

/// Unstage one path, leaving its working-tree change alone.
pub fn unstage(root: &Path, relative: &str) -> Result<(), String> {
    local_git(root)?
        .unstage(Path::new(relative))
        .map_err(|error| error.to_string())
}

/// Commit what is staged.
pub fn commit(root: &Path, message: &str) -> Result<String, String> {
    let message = message.trim();
    if message.is_empty() {
        return Err("a commit message is required".into());
    }
    let service = local_git(root)?;
    if !service
        .status()
        .map_err(|error| error.to_string())?
        .values()
        .any(|status| status.staged.is_some())
    {
        return Err("nothing is staged; stage the files to include first".into());
    }
    service.commit(message).map_err(|error| error.to_string())
}

/// Local branch names, the current one first.
pub fn branches(root: &Path) -> Result<Vec<String>, String> {
    let current = status(root).map(|status| status.branch).unwrap_or_default();
    let mut names = local_git(root)?
        .branches()
        .map_err(|error| error.to_string())?;
    names.sort_by_key(|name| (*name != current, name.clone()));
    Ok(names)
}

/// Switch to an existing branch.
pub fn switch_branch(root: &Path, name: &str) -> Result<(), String> {
    local_git(root)?
        .switch_branch(name.trim())
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Create a branch at HEAD and switch to it.
pub fn create_branch(root: &Path, name: &str) -> Result<(), String> {
    local_git(root)?
        .create_branch(name.trim())
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Merge a local branch into the current one.
pub fn merge_branch(root: &Path, name: &str) -> Result<(), String> {
    local_git(root)?
        .merge(name.trim())
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Rename a local branch.
pub fn rename_branch(root: &Path, from: &str, to: &str) -> Result<(), String> {
    local_git(root)?
        .rename_branch(from, to.trim())
        .map(drop)
        .map_err(|error| error.to_string())
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|error| format!("git: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_string())
}

/// The set of files the tree renders, for tests.
#[allow(dead_code)]
fn paths(entries: &[FileEntry]) -> BTreeSet<String> {
    entries.iter().map(|entry| entry.path.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-desktop-test-{}",
            forge_types::SessionId::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn paths_that_leave_the_workspace_are_refused() {
        let dir = tempdir();
        fs::write(dir.join("ok.txt"), "hello").unwrap();
        assert!(resolve(&dir, "ok.txt").is_ok());
        assert!(resolve(&dir, "../ok.txt").is_err());
        assert!(resolve(&dir, "/etc/passwd").is_err());
        assert!(resolve(&dir, "missing.txt").is_err());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn deleting_a_symlink_leaves_its_target_alone() {
        let dir = tempdir();
        fs::write(dir.join("target.txt"), "keep").unwrap();
        std::os::unix::fs::symlink("target.txt", dir.join("link.txt")).unwrap();

        delete(&dir, "link.txt").unwrap();

        assert!(!dir.join("link.txt").symlink_metadata().is_ok());
        assert_eq!(fs::read_to_string(dir.join("target.txt")).unwrap(), "keep");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn save_never_writes_through_a_preexisting_temp_symlink() {
        let dir = tempdir();
        fs::write(dir.join("a.txt"), "old").unwrap();
        let file = read(&dir, "a.txt").unwrap();
        let outside = std::env::temp_dir().join(format!(
            "forge-desktop-outside-{}",
            forge_types::SessionId::new_v4()
        ));
        fs::write(&outside, "safe").unwrap();
        let planted = dir.join(format!(".forge-save-{}-0", std::process::id()));
        std::os::unix::fs::symlink(&outside, &planted).unwrap();

        write(&dir, "a.txt", "new", &file.fingerprint, false).unwrap();

        assert_eq!(fs::read_to_string(&outside).unwrap(), "safe");
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "new");
        fs::remove_file(outside).ok();
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_tree_is_depth_first_and_skips_runtime_dirs() {
        let dir = tempdir();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join("src").join("main.rs"), "fn main() {}").unwrap();
        fs::write(dir.join("README.md"), "hello").unwrap();
        fs::write(dir.join("target").join("junk"), "no").unwrap();

        let entries = list(&dir).unwrap();
        let seen = paths(&entries);
        assert!(seen.contains("src"));
        assert!(seen.contains("src/main.rs"));
        assert!(seen.contains("README.md"));
        assert!(!seen.contains("target"));
        assert!(!seen.contains("target/junk"));
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names.first(), Some(&"src"), "directories come first");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn reads_text_and_refuses_binary() {
        let dir = tempdir();
        fs::write(dir.join("a.txt"), "line one\nline two\n").unwrap();
        fs::write(dir.join("a.bin"), [0u8, 1, 2, 3]).unwrap();
        let file = read(&dir, "a.txt").unwrap();
        assert_eq!(file.text, "line one\nline two\n");
        assert!(!file.truncated);
        assert!(read(&dir, "a.bin").unwrap_err().contains("binary"));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn writes_existing_files_and_refuses_escapes() {
        let dir = tempdir();
        fs::write(dir.join("a.txt"), "old\n").unwrap();
        let file = read(&dir, "a.txt").unwrap();
        write(&dir, "a.txt", "new\n", &file.fingerprint, file.crlf).unwrap();
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "new\n");
        assert!(write(&dir, "../a.txt", "x", &file.fingerprint, false).is_err());
        assert!(write(&dir, "missing.txt", "x", &file.fingerprint, false).is_err());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_stale_fingerprint_refuses_the_save() {
        let dir = tempdir();
        fs::write(dir.join("a.txt"), "one\n").unwrap();
        let file = read(&dir, "a.txt").unwrap();
        fs::write(dir.join("a.txt"), "two\n").unwrap();
        let error = write(&dir, "a.txt", "three\n", &file.fingerprint, false).unwrap_err();
        assert!(error.contains("changed on disk"), "{error}");
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "two\n");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn crlf_line_endings_survive_a_save() {
        let dir = tempdir();
        fs::write(dir.join("a.txt"), "one\r\ntwo\r\n").unwrap();
        let file = read(&dir, "a.txt").unwrap();
        assert!(file.crlf);
        assert_eq!(file.text, "one\ntwo\n");
        let saved = write(&dir, "a.txt", "one\ntwo\nthree\n", &file.fingerprint, true).unwrap();
        assert_eq!(
            fs::read(dir.join("a.txt")).unwrap(),
            b"one\r\ntwo\r\nthree\r\n"
        );
        assert_eq!(saved.text, "one\ntwo\nthree\n");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn files_the_editor_cannot_round_trip_are_read_only() {
        let dir = tempdir();
        fs::write(dir.join("bad.txt"), [0xff_u8, 0xfe, b'a']).unwrap();
        let file = read(&dir, "bad.txt").unwrap();
        assert!(file.read_only.is_some());

        fs::write(dir.join("big.txt"), vec![b'a'; MAX_FILE_BYTES + 1]).unwrap();
        let file = read(&dir, "big.txt").unwrap();
        assert!(file.truncated);
        assert!(file.read_only.is_some());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn file_operations_stay_inside_the_workspace() {
        let dir = tempdir();
        fs::write(dir.join("a.txt"), "one\n").unwrap();

        create_file(&dir, "new.txt").unwrap();
        assert!(dir.join("new.txt").is_file());
        assert!(create_file(&dir, "new.txt").is_err());
        assert!(create_file(&dir, "../escape.txt").is_err());
        assert!(create_file(&dir, "missing/child.txt").is_err());

        create_dir(&dir, "sub/deep").unwrap();
        assert!(dir.join("sub/deep").is_dir());
        assert!(create_dir(&dir, "sub/deep").is_err());

        rename(&dir, "a.txt", "sub/a.txt").unwrap();
        assert!(!dir.join("a.txt").exists());
        assert!(dir.join("sub/a.txt").is_file());
        assert!(
            rename(&dir, "sub/a.txt", "sub/deep").is_err(),
            "destination exists"
        );
        assert!(rename(&dir, "sub/a.txt", "../out.txt").is_err());

        delete(&dir, "sub/a.txt").unwrap();
        delete(&dir, "sub/deep").unwrap();
        delete(&dir, "sub").unwrap();
        assert!(!dir.join("sub").exists());
        assert!(delete(&dir, "not-there.txt").is_err());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn an_image_preview_is_magic_checked_and_confined() {
        use base64::Engine;
        let dir = tempdir();
        let image =
            base64::engine::general_purpose::STANDARD.encode(forge_types::sample_png_bytes());
        let refs = store_images(&dir, std::slice::from_ref(&image)).unwrap();
        let preview = read_image(&dir, &refs[0].path).unwrap();
        assert_eq!(preview.mime, "image/png");
        assert_eq!((preview.width, preview.height), (Some(1), Some(1)));
        assert!(preview.data_url.starts_with("data:image/png;base64,"));

        fs::write(dir.join("notes.txt"), "not an image").unwrap();
        assert!(read_image(&dir, "notes.txt").is_err());
        assert!(read_image(&dir, "../outside.png").is_err());
        let outside = tempdir();
        fs::write(outside.join("outside.png"), forge_types::sample_png_bytes()).unwrap();
        assert!(read_image(&dir, outside.join("outside.png").to_str().unwrap()).is_err());
        fs::remove_dir_all(dir).ok();
        fs::remove_dir_all(outside).ok();
    }

    #[test]
    fn image_paste_checks_content_and_confines_storage() {
        use base64::Engine;
        let dir = tempdir();
        assert!(store_images(&dir, &["bm90IGFuIGltYWdl".into()]).is_err());
        assert!(!dir.join(".forge").exists());
        let image =
            base64::engine::general_purpose::STANDARD.encode(forge_types::sample_png_bytes());
        let refs = store_images(&dir, std::slice::from_ref(&image)).unwrap();
        assert_eq!(refs[0].mime, "image/png");
        assert_eq!(refs[0].width, Some(1));
        assert_eq!(
            fs::read(resolve(&dir, &refs[0].path).unwrap()).unwrap(),
            forge_types::sample_png_bytes()
        );
        let outside = tempdir();
        fs::remove_dir_all(dir.join(".forge")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, dir.join(".forge")).unwrap();
            assert!(store_images(&dir, &[image]).is_err());
            assert!(!outside.join("attachments").exists());
        }
        fs::remove_dir_all(dir).ok();
        fs::remove_dir_all(outside).ok();
    }

    #[test]
    fn search_is_case_insensitive_and_skips_binary() {
        let dir = tempdir();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/lib.rs"), "fn main() {}\n// TODO: fix\n").unwrap();
        fs::write(dir.join("notes.md"), "todo: write docs\n").unwrap();
        fs::write(dir.join("blob.bin"), b"todo\0\0\0").unwrap();

        let hits = search(&dir, "ToDo").unwrap();
        let paths: Vec<(&str, usize)> = hits
            .iter()
            .map(|hit| (hit.path.as_str(), hit.line))
            .collect();
        assert_eq!(paths, vec![("src/lib.rs", 2), ("notes.md", 1)]);
        assert!(search(&dir, "   ").unwrap().is_empty());
        assert!(search(&dir, "absent").unwrap().is_empty());
        fs::remove_dir_all(dir).ok();
    }

    fn git_repo() -> PathBuf {
        let dir = tempdir();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "test@forge.invalid"][..],
            &["config", "user.name", "Forge Test"][..],
        ] {
            let status = std::process::Command::new("git")
                .current_dir(&dir)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        }
        dir
    }

    #[test]
    fn staging_and_committing_use_the_workspace_verbatim() {
        let dir = git_repo();
        fs::write(dir.join("a.txt"), "one\n").unwrap();
        assert!(commit(&dir, "nothing yet").is_err());
        assert!(commit(&dir, "   ").is_err());

        stage(&dir, "a.txt").unwrap();
        assert_eq!(status(&dir).unwrap().files[0].status, "A");
        let message = commit(&dir, "add a\n\nbody line").unwrap();
        assert!(message.contains("add a"), "{message}");
        assert!(status(&dir).unwrap().files.is_empty());

        unstage(&dir, "a.txt").unwrap();
        fs::write(dir.join("a.txt"), "two\n").unwrap();
        stage(&dir, "a.txt").unwrap();
        unstage(&dir, "a.txt").unwrap();
        assert_eq!(status(&dir).unwrap().files[0].status, "M");
        assert!(stage(&dir, "../outside.txt").is_err());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn branches_can_be_created_switched_merged_and_renamed() {
        let dir = git_repo();
        fs::write(dir.join("a.txt"), "one\n").unwrap();
        stage(&dir, "a.txt").unwrap();
        commit(&dir, "init").unwrap();

        assert_eq!(branches(&dir).unwrap(), vec!["main"]);
        create_branch(&dir, "feature").unwrap();
        assert_eq!(status(&dir).unwrap().branch, "feature");
        fs::write(dir.join("b.txt"), "two\n").unwrap();
        stage(&dir, "b.txt").unwrap();
        commit(&dir, "feature work").unwrap();

        switch_branch(&dir, "main").unwrap();
        assert_eq!(status(&dir).unwrap().branch, "main");
        assert!(!dir.join("b.txt").exists());
        merge_branch(&dir, "feature").unwrap();
        assert!(dir.join("b.txt").exists());

        rename_branch(&dir, "feature", "feature-2").unwrap();
        assert!(branches(&dir).unwrap().contains(&"feature-2".to_string()));
        assert!(switch_branch(&dir, "nope").is_err());
        assert!(create_branch(&dir, "has space").is_err());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn porcelain_status_maps_to_one_letter_each() {
        let raw = " M src/main.rs\0?? new.txt\0A  staged.rs\0R  moved.rs\0old.rs\0UU conflict.rs\0";
        let parsed = parse_status(raw);
        let summary: Vec<(&str, &str, bool, bool)> = parsed
            .iter()
            .map(|file| {
                (
                    file.path.as_str(),
                    file.status.as_str(),
                    file.staged,
                    file.unstaged,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("src/main.rs", "M", false, true),
                ("new.txt", "?", false, true),
                ("staged.rs", "A", true, false),
                ("moved.rs", "R", true, false),
                ("conflict.rs", "U", true, true),
            ]
        );
    }
}
