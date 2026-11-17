//! Whether a repository's `.forge/local` is Forge's own.
//!
//! Forge keeps a repository's runtime state — the sessions it lists and
//! resumes, their journals — in `.forge/local`, kept out of Git by the
//! repository's exclude file. Exclusion does not stop anyone committing
//! files there (`git add -f`), and a clone then holds them as if this
//! machine had written them: sessions it never ran, histories with words it
//! never said. So a `.forge/local` holding anything Git tracks is not used
//! at all (see `LocalRuntimeStorage`); nor is one whose contents Git cannot
//! vouch for. Asked once per repository per process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Mutex, OnceLock};

/// The runtime directory, relative to the repository root.
const LOCAL_DIR: &str = ".forge/local";

/// Names shown in a notice, at most.
const NAMES_SHOWN: usize = 3;

/// What Git says about `.forge/local`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalDirVerdict {
    /// Git tracks nothing there: it is Forge's own.
    Clean,
    /// Git tracks these files there (relative to the repository root,
    /// sorted): someone committed them.
    Tracked(Vec<PathBuf>),
    /// Git could not say (its error).
    Unknown(String),
}

impl LocalDirVerdict {
    pub(crate) fn is_clean(&self) -> bool {
        matches!(self, Self::Clean)
    }

    /// Why `.forge/local` is not used, for the user; `None` when it is.
    /// Names that came from the repository are escaped.
    pub(crate) fn notice(&self) -> Option<String> {
        let fix = "Runtime state is kept in Forge's application-data directory instead. To use \
                   `.forge/local` again, untrack it (`git rm -r --cached .forge/local`, then \
                   commit) and restart Forge.";
        match self {
            Self::Clean => None,
            Self::Tracked(files) => {
                let mut names: Vec<String> = files
                    .iter()
                    .take(NAMES_SHOWN)
                    .map(|file| format!("{:?}", file.to_string_lossy()))
                    .collect();
                if files.len() > NAMES_SHOWN {
                    names.push("…".into());
                }
                let count = match files.len() {
                    1 => "1 file".to_string(),
                    n => format!("{n} files"),
                };
                Some(format!(
                    "`.forge/local` holds {count} Git tracks ({}): they were committed to the \
                     repository, so they are not this machine's sessions, and Forge does not \
                     use them. {fix}",
                    names.join(", ")
                ))
            }
            Self::Unknown(error) => Some(format!(
                "Forge could not tell whether `.forge/local` holds files committed to the \
                 repository (Git: {}), so it does not use it. {fix}",
                error.escape_debug()
            )),
        }
    }
}

/// Runs Git in a directory with arguments.
pub(crate) type Git<'a> = dyn FnMut(&Path, &[&str]) -> std::io::Result<Output> + 'a;

/// Ask Git, in the repository at `root`, what it tracks in `.forge/local`.
pub(crate) fn check_with(root: &Path, git: &mut Git<'_>) -> LocalDirVerdict {
    let output = match git(root, &["ls-files", "-z", "--", LOCAL_DIR]) {
        Ok(output) => output,
        Err(error) => return LocalDirVerdict::Unknown(format!("could not run git: {error}")),
    };
    if !output.status.success() {
        return LocalDirVerdict::Unknown(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        );
    }
    let mut files: Vec<PathBuf> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(String::from_utf8_lossy(path).into_owned()))
        .collect();
    if files.is_empty() {
        return LocalDirVerdict::Clean;
    }
    files.sort();
    LocalDirVerdict::Tracked(files)
}

/// Verdicts already reached, by repository root.
#[derive(Default)]
pub(crate) struct VerdictCache(Mutex<HashMap<PathBuf, LocalDirVerdict>>);

impl VerdictCache {
    /// The verdict for the repository at `root`: asked of `git` the first
    /// time only.
    pub(crate) fn verdict_with(&self, root: &Path, git: &mut Git<'_>) -> LocalDirVerdict {
        let mut verdicts = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(verdict) = verdicts.get(root) {
            return verdict.clone();
        }
        let verdict = check_with(root, git);
        verdicts.insert(root.to_path_buf(), verdict.clone());
        verdict
    }
}

/// The verdict for the repository at `root`, asked of Git once per process.
pub(crate) fn verdict(root: &Path) -> LocalDirVerdict {
    static CACHE: OnceLock<VerdictCache> = OnceLock::new();
    CACHE
        .get_or_init(VerdictCache::default)
        .verdict_with(root, &mut |dir, args| {
            forge_sandbox::git::command(dir).args(args).output()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    fn output(code: i32, stdout: &[u8], stderr: &[u8]) -> std::io::Result<Output> {
        Ok(Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        })
    }

    #[test]
    fn nothing_tracked_is_clean() {
        let verdict = check_with(Path::new("/repo"), &mut |_, _| output(0, b"", b""));
        assert_eq!(verdict, LocalDirVerdict::Clean);
        assert_eq!(verdict.notice(), None);
    }

    #[test]
    fn git_is_asked_about_forge_local_from_the_repository_root() {
        let mut asked = Vec::new();
        check_with(Path::new("/repo"), &mut |dir, args| {
            asked.push((
                dir.to_path_buf(),
                args.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
            ));
            output(0, b"", b"")
        });
        assert_eq!(
            asked,
            [(
                PathBuf::from("/repo"),
                vec![
                    "ls-files".to_string(),
                    "-z".into(),
                    "--".into(),
                    ".forge/local".into()
                ]
            )]
        );
    }

    #[test]
    fn tracked_files_are_listed_in_order_and_named_in_the_notice() {
        let verdict = check_with(Path::new("/repo"), &mut |_, _| {
            output(
                0,
                b".forge/local/sessions/a.db\0.forge/local/control/sessions.db\0",
                b"",
            )
        });
        assert_eq!(
            verdict,
            LocalDirVerdict::Tracked(vec![
                PathBuf::from(".forge/local/control/sessions.db"),
                PathBuf::from(".forge/local/sessions/a.db"),
            ])
        );
        let notice = verdict.notice().unwrap();
        assert!(notice.contains("2 files Git tracks"), "{notice}");
        assert!(
            notice.contains("\".forge/local/control/sessions.db\""),
            "{notice}"
        );
        assert!(
            notice.contains("git rm -r --cached .forge/local"),
            "{notice}"
        );
        assert!(notice.contains("application-data directory"), "{notice}");
    }

    /// Names come from the repository: at most three are shown, escaped.
    #[test]
    fn the_notice_shows_three_names_escaped() {
        let verdict = check_with(Path::new("/repo"), &mut |_, _| {
            output(
                0,
                b".forge/local/a\x1b[31m\0.forge/local/b\0.forge/local/c\0.forge/local/d\0",
                b"",
            )
        });
        let notice = verdict.notice().unwrap();
        assert!(notice.contains("4 files Git tracks"), "{notice}");
        assert!(!notice.contains('\x1b'), "{notice:?}");
        assert!(notice.contains("\\u{1b}"), "{notice}");
        assert!(notice.contains("\".forge/local/c\""), "{notice}");
        assert!(!notice.contains("forge/local/d"), "{notice}");
        let one = check_with(Path::new("/repo"), &mut |_, _| {
            output(0, b".forge/local/x\0", b"")
        });
        assert!(one.notice().unwrap().contains("1 file Git tracks"));
    }

    /// Fail closed: when Git cannot list what it tracks there, `.forge/local`
    /// is not used either.
    #[test]
    fn when_git_cannot_tell_the_directory_is_not_used() {
        let failed = check_with(Path::new("/repo"), &mut |_, _| {
            output(128, b"", b"fatal: index file corrupt\n")
        });
        assert_eq!(
            failed,
            LocalDirVerdict::Unknown("fatal: index file corrupt".into())
        );
        let notice = failed.notice().unwrap();
        assert!(notice.contains("could not tell"), "{notice}");
        assert!(notice.contains("index file corrupt"), "{notice}");

        let missing = check_with(Path::new("/repo"), &mut |_, _| {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no git"))
        });
        assert!(matches!(missing, LocalDirVerdict::Unknown(_)));
        assert!(!missing.is_clean());
    }

    /// Git's own error text is escaped too (it can quote a committed name).
    #[test]
    fn gits_error_is_escaped_in_the_notice() {
        let failed = check_with(Path::new("/repo"), &mut |_, _| {
            output(128, b"", b"fatal: bad \x1b]0;x\x07 path")
        });
        assert!(!failed.notice().unwrap().contains('\x1b'));
    }

    /// One Git run per repository per process, however many times storage
    /// is set up.
    #[test]
    fn git_runs_once_per_repository() {
        let cache = VerdictCache::default();
        let runs = std::cell::Cell::new(0);
        let mut git = |_: &Path, _: &[&str]| {
            runs.set(runs.get() + 1);
            output(0, b".forge/local/x\0", b"")
        };
        let first = cache.verdict_with(Path::new("/repo"), &mut git);
        let again = cache.verdict_with(Path::new("/repo"), &mut git);
        assert_eq!(first, again);
        let other = cache.verdict_with(Path::new("/other"), &mut git);
        assert_eq!(other, first);
        assert_eq!(runs.get(), 2);
    }
}
