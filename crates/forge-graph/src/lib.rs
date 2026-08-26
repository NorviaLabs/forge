//! Repo-wide symbol graph — `find_definition`/`find_references` backing
//! store. Syntactic only: every fact is a real tree-sitter node, and where a
//! name can't be structurally narrowed to one definition, every real
//! candidate is returned rather than a guess (see `schema.rs`, `store.rs`).
//!
//! Covers the 5 languages `forge-syntax` already tree-sitter-parses for
//! highlighting: Rust, Python, Go, TypeScript, JavaScript.
//!
//! `GraphHandle::open` still synchronously awaits the initial build (a
//! deliberate PR3 simplification, not an oversight — see its doc comment),
//! but that build is now hash-aware: a file whose content hasn't changed
//! since it was last indexed is skipped entirely, so re-opening an
//! already-built repo is fast. After that, a `notify` watcher
//! (`watcher.rs`) keeps the graph live for the rest of the process:
//! changes are debounced and re-indexed one file at a time, accepting the
//! staleness window documented there rather than blocking on it.

mod extract;
pub mod schema;
mod store;
mod watcher;

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

pub use extract::{EdgeFact, ExtractedFacts, Extractor, SymbolFact};
pub use store::{GraphError, GraphStore, ReferenceMatch, SymbolMatch};

/// Directories never walked when building the graph — generated code,
/// dependencies, Forge's own runtime state, and any other tool's scratch
/// area. Mirrors the same category of skip-list `forge-tui`'s file watcher
/// already hardcodes for `.git`/`.forge`
/// (`crates/forge-tui/src/app/watch.rs`), extended with build-output/
/// dependency directories a symbol graph has no reason to enter.
///
/// This is a hardcoded approximation, not real `.gitignore` compliance —
/// found the hard way: an earlier dogfood run against forge's own repo
/// indexed four copies of every symbol because `.claude/worktrees/` (this
/// harness's own gitignored scratch worktrees, full checkouts of the repo)
/// wasn't excluded, and every duplicate read as a genuine repo-wide
/// ambiguity. A future revision should walk via the same gitignore-aware
/// crate `fff_search` already uses instead of maintaining this list by
/// hand as new cases turn up.
pub(crate) const SKIP_DIRS: &[&str] = &[
    ".git",
    ".forge",
    ".claude",
    "target",
    "node_modules",
    ".venv",
    "vendor",
    "dist",
    "build",
    "__pycache__",
];

pub struct GraphHandle {
    store: Arc<GraphStore>,
    workspace: PathBuf,
    watcher: Mutex<Option<watcher::WatcherGuard>>,
}

impl GraphHandle {
    /// Opens (or creates) the persisted store for `workspace`, runs the
    /// (hash-aware, so a re-open of an unchanged repo is fast) initial
    /// build, then starts the live watcher. `db_path` is passed in rather
    /// than resolved here: `forge-graph` doesn't depend on
    /// `forge-storage`'s `RuntimeDataKind` directly, to keep this crate
    /// storage-location-agnostic — the caller (`forge-session`) resolves
    /// the path via `RuntimeDataKind::Graph`.
    ///
    /// Deliberately still synchronous on the *initial* build, not spawned
    /// in the background: doing that safely means every query has to
    /// account for "the graph isn't built yet" as a third answer alongside
    /// "found" and "not found" — real complexity for a case the hash-skip
    /// optimization already makes cheap after the first real build. The
    /// "no synchronous block" requirement from the design review was about
    /// re-indexing *after* an edit mid-session, which the watcher below
    /// satisfies — not about the very first build.
    pub async fn open(workspace: &Path, db_path: &Path) -> Result<Self, GraphError> {
        let store = Arc::new(GraphStore::open(db_path).await?);
        Self::from_store(workspace, store).await
    }

    /// In-memory store, immediately built from `workspace` — for tests.
    pub async fn open_in_memory(workspace: &Path) -> Result<Self, GraphError> {
        let store = Arc::new(GraphStore::open_in_memory().await?);
        Self::from_store(workspace, store).await
    }

    async fn from_store(workspace: &Path, store: Arc<GraphStore>) -> Result<Self, GraphError> {
        build_full(&store, workspace).await?;
        let watcher = watcher::start(workspace.to_path_buf(), store.clone()).ok();
        Ok(Self {
            store,
            workspace: workspace.to_path_buf(),
            watcher: Mutex::new(watcher),
        })
    }

    pub fn store(&self) -> &GraphStore {
        &self.store
    }

    pub async fn find_definition(&self, name: &str) -> Result<Vec<SymbolMatch>, GraphError> {
        self.store.find_definition(name).await
    }

    pub async fn find_references(&self, name: &str) -> Result<Vec<ReferenceMatch>, GraphError> {
        self.store.find_references(name).await
    }

    /// Stops the live watcher without touching the persisted store — the
    /// `/graph disable` case. A no-op if already paused.
    pub async fn pause_watcher(&self) {
        *self.watcher.lock().await = None;
    }

    /// Restarts the watcher after `pause_watcher`, first running a cheap
    /// catch-up sweep: every previously-indexed file whose on-disk content
    /// hash no longer matches what's recorded gets re-indexed, so edits
    /// made while paused aren't silently missed. Cheaper than a full
    /// rebuild — unchanged files are skipped, exactly like `open`'s
    /// initial build. A no-op if already watching.
    pub async fn resume_watcher(&self) -> Result<(), GraphError> {
        let mut guard = self.watcher.lock().await;
        if guard.is_some() {
            return Ok(());
        }
        catch_up(&self.store, &self.workspace).await?;
        *guard = watcher::start(self.workspace.clone(), self.store.clone()).ok();
        Ok(())
    }

    pub fn is_watching(&self) -> bool {
        // `try_lock` rather than blocking: this is a status read, and the
        // watcher lock is only ever held briefly during pause/resume.
        self.watcher.try_lock().map(|g| g.is_some()).unwrap_or(true)
    }
}

/// Re-indexes every file whose recorded content hash no longer matches
/// disk — used both by the initial build (via `build_full`, where nothing
/// is recorded yet so everything "changes") and by `resume_watcher`'s
/// catch-up sweep.
async fn build_full(store: &GraphStore, workspace: &Path) -> Result<(), GraphError> {
    let mut symbols_by_lang: HashMap<&'static str, Vec<SymbolFact>> = HashMap::new();
    let mut all_edges = Vec::new();
    let mut to_record: Vec<(String, &'static str, String)> = Vec::new();

    for path in walk_files(workspace) {
        let Some((lang, facts, hash)) = extract_file(&path, workspace) else {
            continue;
        };
        let rel = rel_path(&path, workspace);
        if store.file_content_hash(&rel).await? == Some(hash.clone()) {
            continue; // byte-identical since last index — nothing to redo
        }
        store.clear_file(&rel).await?;
        symbols_by_lang
            .entry(lang)
            .or_default()
            .extend(facts.symbols);
        all_edges.extend(facts.edges);
        to_record.push((rel, lang, hash));
    }

    for (lang, symbols) in &symbols_by_lang {
        store.insert_symbols(lang, symbols).await?;
    }
    store.resolve_and_insert_edges(&all_edges).await?;
    for (rel, lang, hash) in &to_record {
        store.upsert_file(rel, lang, hash).await?;
    }
    Ok(())
}

/// Same hash-skip logic as `build_full`, exposed under the name the
/// `resume_watcher` catch-up sweep call site reads more naturally under.
async fn catch_up(store: &GraphStore, workspace: &Path) -> Result<(), GraphError> {
    build_full(store, workspace).await
}

/// The watcher's unit of work: re-index exactly one changed file,
/// immediately, against whatever the rest of the graph currently holds.
/// This is *not* how the initial build (`build_full`) works — that batches
/// every changed file's symbols before resolving any edges, so a
/// same-build cross-file reference resolves correctly. Here, on an
/// already-built graph, immediate single-file resolution is the documented
/// v1 trade-off: this file's own outgoing edges are current the instant
/// this returns, but another file's *existing* edge into a symbol this
/// file just changed stays stale until that other file is itself
/// re-indexed. `clear_file` still keeps that stale edge honest rather than
/// wrong — deleting a symbol deletes every edge that pointed at it, so a
/// stale reference reads as "not found," never as a phantom hit.
pub(crate) async fn reindex_one_file(
    store: &GraphStore,
    workspace: &Path,
    path: &Path,
) -> Result<(), GraphError> {
    let rel = rel_path(path, workspace);
    store.clear_file(&rel).await?;
    if !path.exists() {
        return Ok(()); // deletion: clearing its rows is the whole job
    }
    let Some((lang, facts, hash)) = extract_file(path, workspace) else {
        return Ok(()); // unrecognized/unreadable — nothing further to record
    };
    store.insert_symbols(lang, &facts.symbols).await?;
    store.resolve_and_insert_edges(&facts.edges).await?;
    store.upsert_file(&rel, lang, &hash).await?;
    Ok(())
}

/// Relative path used as the `symbols`/`edges`/`files` key. Both sides are
/// canonicalized first: on macOS, `notify` (via FSEvents) reports paths
/// through `/private/var/...`, while a caller's `workspace` root — e.g. a
/// `tempdir()` in tests, or `/var/...`-style paths generally — resolves
/// through a symlink to the same place. Without canonicalizing,
/// `strip_prefix` silently fails, the watcher records edits under the
/// absolute path instead of the file's real relative one, and the initial
/// build's rows for that file are never matched (so never cleared) again.
pub(crate) fn rel_path(path: &Path, workspace: &Path) -> String {
    let canon_workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    // `path` may already be gone (a deletion event) — canonicalize can't
    // resolve a symlink component in a path that no longer exists, so fall
    // back to canonicalizing its still-present parent and reattaching the
    // file name, which is enough to normalize the same symlink prefix.
    let canon_path =
        path.canonicalize()
            .unwrap_or_else(|_| match (path.parent(), path.file_name()) {
                (Some(parent), Some(name)) => parent
                    .canonicalize()
                    .map(|p| p.join(name))
                    .unwrap_or_else(|_| path.to_path_buf()),
                _ => path.to_path_buf(),
            });
    canon_path
        .strip_prefix(&canon_workspace)
        .unwrap_or(&canon_path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn content_hash(source: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Reads and extracts one file, if its extension maps to a supported
/// language and it decodes as UTF-8 text. `None` for anything else —
/// unrecognized extensions, binaries, and unreadable files are silently
/// skipped, not errors: most of a real repo isn't source code in one of
/// these 5 languages.
fn extract_file(path: &Path, workspace: &Path) -> Option<(&'static str, ExtractedFacts, String)> {
    let ext = path.extension()?.to_str()?;
    let source = std::fs::read_to_string(path).ok()?;
    let rel_string = rel_path(path, workspace);
    let rel = Path::new(&rel_string);
    let (lang, facts) = match ext {
        "rs" => ("rust", extract::rust::RustExtractor.extract(&source, rel)),
        "py" | "pyi" => (
            "python",
            extract::python::PythonExtractor.extract(&source, rel),
        ),
        "go" => ("go", extract::go::GoExtractor.extract(&source, rel)),
        "ts" | "tsx" => (
            "typescript",
            extract::typescript::TypeScriptExtractor.extract(&source, rel),
        ),
        "js" | "jsx" | "mjs" | "cjs" => (
            "javascript",
            extract::javascript::JavaScriptExtractor.extract(&source, rel),
        ),
        _ => return None,
    };
    Some((lang, facts, content_hash(&source)))
}

/// True if any component of `path` names a skipped directory — the
/// watcher's per-event version of the check `walk_dir` applies while
/// descending. Checks every component, not just the immediate parent,
/// since a `notify` event's path is absolute and may be several directories
/// below the workspace root.
pub(crate) fn is_skipped_path(path: &Path) -> bool {
    path.components().any(|c| match c.as_os_str().to_str() {
        Some(name) => SKIP_DIRS.contains(&name),
        None => false,
    })
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_dir(root, &mut out);
    out
}

fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            walk_dir(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn builds_from_a_real_small_workspace_and_answers_queries() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn helper() {}\n\npub fn entry() {\n    helper();\n}\n",
        )
        .unwrap();

        let handle = GraphHandle::open_in_memory(dir.path()).await.unwrap();

        let def = handle.find_definition("helper").await.unwrap();
        assert_eq!(def.len(), 1);
        assert_eq!(def[0].file, "src/lib.rs");

        let refs = handle.find_references("helper").await.unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].from_line, 4);
    }

    /// A polyglot repo — the real shape a build has to handle once it's not
    /// Rust-only: each language's file is parsed by its own grammar, all
    /// five land in one shared graph, and a name unique to one language
    /// resolves without leaking into or colliding with the others.
    #[tokio::test]
    async fn builds_across_all_five_languages_in_one_repo() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("main.rs"), "pub fn rust_only() {}\n").unwrap();
        fs::write(dir.path().join("main.py"), "def python_only():\n    pass\n").unwrap();
        fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc GoOnly() {}\n",
        )
        .unwrap();
        fs::write(dir.path().join("main.ts"), "function typescriptOnly() {}\n").unwrap();
        fs::write(dir.path().join("main.js"), "function javascriptOnly() {}\n").unwrap();

        let handle = GraphHandle::open_in_memory(dir.path()).await.unwrap();

        for name in [
            "rust_only",
            "python_only",
            "GoOnly",
            "typescriptOnly",
            "javascriptOnly",
        ] {
            let def = handle.find_definition(name).await.unwrap();
            assert_eq!(
                def.len(),
                1,
                "{name} should resolve to exactly one definition"
            );
        }
    }

    #[tokio::test]
    async fn skips_target_and_git_directories() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        fs::write(
            dir.path().join("target/debug/generated.rs"),
            "pub fn should_not_be_indexed() {}\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "pub fn real() {}\n").unwrap();

        let handle = GraphHandle::open_in_memory(dir.path()).await.unwrap();
        assert!(handle
            .find_definition("should_not_be_indexed")
            .await
            .unwrap()
            .is_empty());
        assert_eq!(handle.find_definition("real").await.unwrap().len(), 1);
    }

    /// Regression for a real dogfood-run bug: a gitignored scratch worktree
    /// checked out inside the workspace (as `.claude/worktrees/<id>/` is in
    /// this very repo) is a full copy of the source tree. Walking into it
    /// turned every real symbol into a false multi-candidate ambiguity.
    #[tokio::test]
    async fn skips_gitignored_scratch_worktrees() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "pub fn real_one() {}\n").unwrap();
        fs::create_dir_all(dir.path().join(".claude/worktrees/agent-x/src")).unwrap();
        fs::write(
            dir.path().join(".claude/worktrees/agent-x/src/lib.rs"),
            "pub fn real_one() {}\n",
        )
        .unwrap();

        let handle = GraphHandle::open_in_memory(dir.path()).await.unwrap();
        assert_eq!(
            handle.find_definition("real_one").await.unwrap().len(),
            1,
            "a scratch worktree copy must not be indexed as a second real definition"
        );
    }

    /// The one test exercising `watcher.rs` for real: edit a fixture file on
    /// disk after the initial build, poll (never `sleep`-and-hope) past the
    /// debounce window, and confirm the store picked up the change without
    /// any caller-driven re-index call.
    #[tokio::test]
    async fn watcher_picks_up_an_on_disk_edit_without_a_manual_reindex() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        let file = dir.path().join("src/lib.rs");
        fs::write(&file, "pub fn before_edit() {}\n").unwrap();

        let handle = GraphHandle::open_in_memory(dir.path()).await.unwrap();
        assert_eq!(
            handle.find_definition("before_edit").await.unwrap().len(),
            1
        );

        fs::write(&file, "pub fn after_edit() {}\n").unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if !handle
                .find_definition("after_edit")
                .await
                .unwrap()
                .is_empty()
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "watcher did not pick up the edit within 5s"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            handle
                .find_definition("before_edit")
                .await
                .unwrap()
                .is_empty(),
            "the old symbol should be gone once the file was re-indexed"
        );
    }

    /// `pause_watcher`/`resume_watcher` — the `/graph off` then `/graph on`
    /// path. An edit made while paused is missed live, but the catch-up
    /// sweep on resume picks it up without a full rebuild.
    #[tokio::test]
    async fn pause_then_resume_catches_up_on_edits_made_while_paused() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        let file = dir.path().join("src/lib.rs");
        fs::write(&file, "pub fn before_pause() {}\n").unwrap();

        let handle = GraphHandle::open_in_memory(dir.path()).await.unwrap();
        assert!(handle.is_watching());

        handle.pause_watcher().await;
        assert!(!handle.is_watching());

        fs::write(&file, "pub fn while_paused() {}\n").unwrap();
        // Give a real watcher every chance to (wrongly) fire while paused.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(
            handle
                .find_definition("while_paused")
                .await
                .unwrap()
                .is_empty(),
            "a paused watcher must not react to filesystem events"
        );

        handle.resume_watcher().await.unwrap();
        assert!(handle.is_watching());
        assert_eq!(
            handle.find_definition("while_paused").await.unwrap().len(),
            1,
            "resume's catch-up sweep should index edits made while paused"
        );
    }
}
