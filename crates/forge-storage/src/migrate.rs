//! One-time, conservative migration of known Forge-generated runtime files
//! from their legacy `.forge/...` locations into `.forge/local/...`.
//!
//! Project-owned resources (`.forge/rules|agents|skills|workflows`) and any
//! unrecognized `.forge` content are never touched — only the specific
//! legacy paths this module lists by name, and only when they are not
//! tracked by Git and the destination has no existing content.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MigrationOutcome {
    /// Moved from the legacy path to its `.forge/local/...` destination.
    Migrated,
    /// Nothing to do — the legacy path doesn't exist.
    NotPresent,
    /// Skipped: the legacy path is tracked by Git. Forge never moves,
    /// untracks, or deletes tracked content automatically.
    Tracked,
    /// Skipped: the destination already has content — never overwritten silently.
    DestinationCollision,
    /// Attempted but failed; the source is left untouched (`fs::rename`
    /// either succeeds atomically or leaves the source exactly as it was).
    Failed,
}

#[derive(Debug, Clone)]
pub struct MigrationRecord {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub outcome: MigrationOutcome,
    pub detail: Option<String>,
}

struct LegacyCandidate {
    relative_source: &'static str,
    relative_destination: &'static str,
}

/// Every legacy path this codebase has historically written directly under
/// `.forge/`, and where it now belongs under `.forge/local/`. Deliberately a
/// fixed, explicit list — an unrecognized `.forge` entry is never inferred
/// to be a migration candidate.
const CANDIDATES: &[LegacyCandidate] = &[
    LegacyCandidate {
        relative_source: "sessions",
        relative_destination: "sessions",
    },
    LegacyCandidate {
        relative_source: "ui-state.json",
        relative_destination: "ui-state/ui-state.json",
    },
    LegacyCandidate {
        relative_source: "run-history.json",
        relative_destination: "ui-state/run-history.json",
    },
    LegacyCandidate {
        relative_source: "progress.json",
        relative_destination: "checkpoints/progress.json",
    },
    LegacyCandidate {
        relative_source: "offload",
        relative_destination: "cache",
    },
];

/// Conservatively migrate known, untracked, Forge-generated runtime files
/// from legacy `.forge/...` locations into `.forge/local/...`. Idempotent —
/// safe to call every time repository-local storage is set up: a path with
/// nothing left to migrate resolves to `NotPresent` cheaply (a single
/// `exists()` check, no Git subprocess).
pub fn migrate_legacy_runtime_files(workspace: &Path) -> Vec<MigrationRecord> {
    let legacy_root = workspace.join(".forge");
    let local_root = legacy_root.join("local");
    let mut records = Vec::new();

    for candidate in CANDIDATES {
        let source = legacy_root.join(candidate.relative_source);
        let destination = local_root.join(candidate.relative_destination);
        records.push(migrate_one(workspace, source, destination));
    }

    records
}

fn migrate_one(workspace: &Path, source: PathBuf, destination: PathBuf) -> MigrationRecord {
    if !source.exists() {
        return MigrationRecord {
            source,
            destination,
            outcome: MigrationOutcome::NotPresent,
            detail: None,
        };
    }

    if is_tracked(workspace, &source) {
        return MigrationRecord {
            source,
            destination,
            outcome: MigrationOutcome::Tracked,
            detail: Some(
                "tracked by Git — Forge did not modify the index; review before migrating manually"
                    .into(),
            ),
        };
    }

    if destination.exists() {
        return MigrationRecord {
            source,
            destination,
            outcome: MigrationOutcome::DestinationCollision,
            detail: Some("destination already has content; left both in place".into()),
        };
    }

    let Some(parent) = destination.parent() else {
        return MigrationRecord {
            source,
            destination,
            outcome: MigrationOutcome::Failed,
            detail: Some("destination has no parent directory".into()),
        };
    };
    if let Err(err) = fs::create_dir_all(parent) {
        return MigrationRecord {
            source,
            destination,
            outcome: MigrationOutcome::Failed,
            detail: Some(err.to_string()),
        };
    }

    match fs::rename(&source, &destination) {
        Ok(()) => MigrationRecord {
            source,
            destination,
            outcome: MigrationOutcome::Migrated,
            detail: None,
        },
        Err(err) => MigrationRecord {
            source,
            destination,
            outcome: MigrationOutcome::Failed,
            detail: Some(err.to_string()),
        },
    }
}

/// True if `path` (relative to `workspace`) is tracked by Git — checked via
/// `git ls-files`, never by inspecting `.git` internals directly.
fn is_tracked(workspace: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(workspace) else {
        return false;
    };
    match Command::new("git")
        .arg("-C")
        .arg(workspace)
        .arg("ls-files")
        .arg("--")
        .arg(rel)
        .output()
    {
        Ok(out) => out.status.success() && !out.stdout.is_empty(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_repo(dir: &Path) {
        for args in [
            vec!["init", "--initial-branch=main", "-q"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(&args)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

    #[test]
    fn migrates_untracked_known_runtime_files() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        fs::create_dir_all(dir.path().join(".forge/sessions")).unwrap();
        fs::write(dir.path().join(".forge/sessions/abc.db"), "data").unwrap();
        fs::write(dir.path().join(".forge/ui-state.json"), "{}").unwrap();

        let records = migrate_legacy_runtime_files(dir.path());

        let sessions = records
            .iter()
            .find(|r| r.source.ends_with("sessions"))
            .unwrap();
        assert_eq!(sessions.outcome, MigrationOutcome::Migrated);
        assert!(dir.path().join(".forge/local/sessions/abc.db").is_file());
        assert!(!dir.path().join(".forge/sessions").exists());

        let ui_state = records
            .iter()
            .find(|r| r.source.ends_with("ui-state.json"))
            .unwrap();
        assert_eq!(ui_state.outcome, MigrationOutcome::Migrated);
        assert!(dir
            .path()
            .join(".forge/local/ui-state/ui-state.json")
            .is_file());
    }

    #[test]
    fn leaves_forge_rules_and_skills_unchanged() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        fs::create_dir_all(dir.path().join(".forge/rules")).unwrap();
        fs::write(dir.path().join(".forge/rules/style.md"), "rules").unwrap();
        fs::create_dir_all(dir.path().join(".forge/skills/ponytail")).unwrap();
        fs::write(dir.path().join(".forge/skills/ponytail/SKILL.md"), "skill").unwrap();

        migrate_legacy_runtime_files(dir.path());

        assert!(dir.path().join(".forge/rules/style.md").is_file());
        assert!(dir.path().join(".forge/skills/ponytail/SKILL.md").is_file());
    }

    #[test]
    fn leaves_unknown_forge_content_unchanged_and_unreported() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        fs::create_dir_all(dir.path().join(".forge")).unwrap();
        fs::write(dir.path().join(".forge/custom.toml"), "x").unwrap();

        let records = migrate_legacy_runtime_files(dir.path());

        assert!(dir.path().join(".forge/custom.toml").is_file());
        assert!(!records.iter().any(|r| r.source.ends_with("custom.toml")));
    }

    #[test]
    fn detects_tracked_runtime_files_and_does_not_move_them() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        fs::create_dir_all(dir.path().join(".forge")).unwrap();
        fs::write(dir.path().join(".forge/progress.json"), "{}").unwrap();
        let status = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["add", ".forge/progress.json"])
            .status()
            .unwrap();
        assert!(status.success());

        let records = migrate_legacy_runtime_files(dir.path());

        let progress = records
            .iter()
            .find(|r| r.source.ends_with("progress.json"))
            .unwrap();
        assert_eq!(progress.outcome, MigrationOutcome::Tracked);
        assert!(dir.path().join(".forge/progress.json").is_file());
        assert!(!dir
            .path()
            .join(".forge/local/checkpoints/progress.json")
            .exists());
    }

    #[test]
    fn does_not_overwrite_existing_destination_content() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        fs::create_dir_all(dir.path().join(".forge")).unwrap();
        fs::write(dir.path().join(".forge/progress.json"), "old").unwrap();
        fs::create_dir_all(dir.path().join(".forge/local/checkpoints")).unwrap();
        fs::write(
            dir.path().join(".forge/local/checkpoints/progress.json"),
            "new",
        )
        .unwrap();

        let records = migrate_legacy_runtime_files(dir.path());

        let progress = records
            .iter()
            .find(|r| r.source.ends_with("progress.json"))
            .unwrap();
        assert_eq!(progress.outcome, MigrationOutcome::DestinationCollision);
        // Both copies survive untouched.
        assert_eq!(
            fs::read_to_string(dir.path().join(".forge/progress.json")).unwrap(),
            "old"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join(".forge/local/checkpoints/progress.json")).unwrap(),
            "new"
        );
    }

    #[test]
    fn is_idempotent() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        fs::create_dir_all(dir.path().join(".forge")).unwrap();
        fs::write(dir.path().join(".forge/ui-state.json"), "{}").unwrap();

        let first = migrate_legacy_runtime_files(dir.path());
        assert!(first.iter().any(
            |r| r.source.ends_with("ui-state.json") && r.outcome == MigrationOutcome::Migrated
        ));

        let second = migrate_legacy_runtime_files(dir.path());
        let ui_state = second
            .iter()
            .find(|r| r.destination.ends_with("ui-state.json"))
            .unwrap();
        assert_eq!(ui_state.outcome, MigrationOutcome::NotPresent);
    }

    #[test]
    fn reports_not_present_when_nothing_to_migrate() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let records = migrate_legacy_runtime_files(dir.path());
        assert!(records
            .iter()
            .all(|r| r.outcome == MigrationOutcome::NotPresent));
    }
}
