//! The permission model's contract, written as a table.
//!
//! This is the harness the model is meant to be held to. It exists because of
//! how the real defects in this area actually presented, none of which a
//! per-function unit test would have caught:
//!
//! * **A rule that was present and silently inert.** Paths were not
//!   canonicalised, so the `.git` deny was written against `/var/…` while the
//!   kernel matched `/private/var/…`. The profile read correctly and the
//!   protection simply did not exist.
//! * **A rule that aborted the whole invocation.** `--tmpfs /var/run` fails on
//!   a symlink, so every sandboxed command failed — found only by running one.
//! * **Ordering.** Masking `/tmp` after binding the workspace hid workspaces
//!   living under `/tmp`.
//! * **A cross-layer gap.** Deleting the read-only classifier removed prompts
//!   the gate was relying on it to avoid.
//!
//! Three properties follow from that, and this file is built around them:
//!
//! 1. **Assert on behaviour, never on configuration.** Every case spawns a real
//!    process and checks what the kernel did. A well-formed invocation and a
//!    working one are different things.
//! 2. **State the contract in one readable place.** A table someone can scan to
//!    see what forge promises, and extend in one line.
//! 3. **Never skip silently.** A host that cannot confine fails the suite in
//!    CI rather than reporting green while asserting nothing.

use std::path::Path;
use std::process::Command;

use forge_tools::sandbox::{availability, wrap_shell_command, SandboxPolicy};

/// What the sandbox must do with a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// Runs to completion. The agent's ordinary working case.
    Allowed,
    /// The kernel refuses. Not "forge declines to run it" — the process starts
    /// and is stopped by the OS.
    Denied,
    /// The command may succeed, but nothing it wrote is visible on the host.
    ///
    /// This distinction is not pedantry, and the platforms genuinely differ.
    /// Linux masks `/tmp` with a private tmpfs so the sandbox has somewhere to
    /// write; a workspace under `/tmp` therefore has a *writable* parent, and
    /// `..` lands in that private filesystem rather than being refused. macOS
    /// has no such mask and refuses outright.
    ///
    /// Both satisfy the property that actually matters — the host is
    /// untouched — and asserting `Denied` on both would have forced either a
    /// platform-conditional table or a read-only `/tmp` that breaks every
    /// toolchain needing scratch space. The contract states the guarantee, not
    /// the mechanism that delivers it.
    ContainedFromHost { host_path: &'static str },
}

/// One row of the contract.
struct Case {
    /// What a reader should understand this row to mean.
    name: &'static str,
    /// Shell run inside the workspace. `{outside}` is replaced with a path
    /// outside the workspace that the test owns.
    command: &'static str,
    expect: Expect,
    /// Why the contract says this, so a failure explains itself rather than
    /// leaving the next person to reconstruct the intent.
    why: &'static str,
}

/// The contract.
///
/// Read top to bottom, this is what forge promises about an agent-run command.
const CONTRACT: &[Case] = &[
    // ---- the ordinary working case -------------------------------------
    Case {
        name: "write inside the workspace",
        command: "echo hello > file.txt",
        expect: Expect::Allowed,
        why: "the workspace is the one writable place; without this the agent cannot work",
    },
    Case {
        name: "read inside the workspace",
        command: "cat marker.txt",
        expect: Expect::Allowed,
        why: "reading the project is the most common thing an agent does",
    },
    Case {
        name: "create nested directories",
        command: "mkdir -p a/b/c && touch a/b/c/f",
        expect: Expect::Allowed,
        why: "ordinary project work, and it exercises directory creation rather than file writes",
    },
    Case {
        name: "destroy source inside the workspace",
        command: "rm -rf src",
        expect: Expect::Allowed,
        why: "deliberate: in-workspace damage is recoverable and visible, and blocking it \
              would mean classifying destructiveness, which is unbounded",
    },
    // ---- the perimeter --------------------------------------------------
    Case {
        name: "write outside the workspace",
        command: "echo pwned > {outside}/escape.txt",
        expect: Expect::Denied,
        why: "the perimeter; everything else in this file is a variation on getting past it",
    },
    Case {
        name: "write outside via cd",
        command: "cd {outside} && echo pwned > escape.txt",
        expect: Expect::Denied,
        why: "the boundary is the path, not the working directory — the distinction \
              `current_dir()` alone never provided",
    },
    Case {
        name: "write outside via parent traversal",
        command: "echo pwned > ../escape-traversal.txt",
        expect: Expect::ContainedFromHost {
            host_path: "../escape-traversal.txt",
        },
        why: "`..` must not reach the host. On Linux it lands in the sandbox's private tmpfs \
              over /tmp and the write succeeds harmlessly; on macOS it is refused. Either way \
              the file must not exist on the host afterwards",
    },
    Case {
        name: "append outside the workspace",
        command: "printf x >> {outside}/escape-append.txt",
        expect: Expect::Denied,
        why: "append is a different open() mode than truncate and could be gated separately",
    },
    Case {
        name: "write outside via tee",
        command: "echo pwned | tee {outside}/escape-tee.txt",
        expect: Expect::Denied,
        why: "a different process doing the writing must not change the answer",
    },
    Case {
        name: "read outside the workspace",
        command: "cat {outside}/credentials.txt",
        expect: Expect::Denied,
        why: "reads are confined to the workspace + session temp, exactly as read_file and \
              write_file confine them. The path is unreachable: masked on Linux, refused \
              by Seatbelt on macOS. ~/.ssh and ~/.aws live in a directory like {outside}",
    },
    // ---- the recovery mechanism protects itself -------------------------
    Case {
        name: "write .git",
        command: "echo clobbered > .git/HEAD",
        expect: Expect::Denied,
        why: "git is the recovery this model leans on for in-workspace damage; an agent that \
              can delete .git deletes the recovery",
    },
    Case {
        name: "write .forge/permissions.toml",
        command: "echo 'allow = [\"*\"]' > .forge/permissions.toml",
        expect: Expect::Denied,
        why: "otherwise a confined process widens its own permissions on the next load, using \
              nothing but an ordinary in-workspace write",
    },
    Case {
        name: "delete .git entirely",
        command: "rm -rf .git",
        expect: Expect::Denied,
        why: "the carve-out must survive removal of the directory, not only writes into it",
    },
    // ---- network --------------------------------------------------------
    Case {
        name: "network egress without a grant",
        command: "curl -sS -m 5 https://example.com",
        expect: Expect::Denied,
        why: "no grant means no route out; the filesystem boundary and the network denial \
              are independent, and both default to closed",
    },
];

/// Deliberate attempts to get around the rules above.
///
/// Split out because these are the cases a reader should be able to find
/// quickly when asking "but what about…". Every one of them is a way a
/// path-based rule can be true of the string and false of the target.
const ESCAPES: &[Case] = &[
    Case {
        name: "write through a symlink pointing outside",
        command: "ln -s {outside} link && echo pwned > link/escape-symlink.txt",
        expect: Expect::Denied,
        why: "the sandbox must resolve the target, not the path written. A rule that matched \
              the visible path would allow this, and the workspace bind makes the link itself \
              legal to create",
    },
    Case {
        name: "write .git through a symlink",
        command: "ln -s .git gitlink && echo clobbered > gitlink/HEAD",
        expect: Expect::Denied,
        why: "the read-only carve-out must survive indirection, or protecting .git is cosmetic",
    },
    Case {
        name: "write outside via a symlinked parent directory",
        command: "mkdir -p nest && ln -s {outside} nest/out && echo pwned > nest/out/escape.txt",
        expect: Expect::Denied,
        why: "the escaping component can be anywhere in the path, not only the last one",
    },
    Case {
        name: "write outside via an absolute path built at runtime",
        command: "target={outside}/escape-runtime.txt; echo pwned > \"$target\"",
        expect: Expect::Denied,
        why: "the enforcement cannot depend on the destination being visible in the command \
              text — which is the whole reason a classifier was the wrong boundary",
    },
    Case {
        name: "write outside from a subshell",
        command: "(cd {outside} && echo pwned > escape-subshell.txt)",
        expect: Expect::Denied,
        why: "confinement is inherited by children; a subshell is not an escape hatch",
    },
    Case {
        name: "write outside via a second interpreter",
        command: "sh -c 'echo pwned > {outside}/escape-nested.txt'",
        expect: Expect::Denied,
        why: "spawning another shell must not shed the sandbox",
    },
];

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A workspace shaped like a real project: a git repo, a forge directory, and
/// something to read.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(dir.path().join(".forge/permissions.toml"), "allow = []\n").unwrap();
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(dir.path().join("marker.txt"), "marker\n").unwrap();
    dir
}

/// Run `command` confined to `root`, returning whether it succeeded.
fn run(root: &Path, command: &str) -> (bool, String) {
    let policy = SandboxPolicy::for_workspace(root);
    let (program, args) = wrap_shell_command("sh", command, &policy)
        .expect("the sandbox must be available; see require_sandbox");
    let out = Command::new(program)
        .args(args)
        .current_dir(root)
        .output()
        .expect("spawn the sandbox wrapper");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// Fail rather than skip when the host cannot confine.
///
/// A silent skip is how a suite keeps reporting green while testing nothing —
/// which happened here: a flaky test in another crate stopped cargo before this
/// suite ran at all, and three commits went by believing Linux was covered.
fn require_sandbox() -> bool {
    match availability() {
        Ok(()) => true,
        Err(unavailable) => {
            if std::env::var_os("CI").is_some() {
                panic!(
                    "CI must be able to confine, or this suite asserts nothing — {}",
                    unavailable.reason()
                );
            }
            eprintln!("SKIP permission contract: {}", unavailable.reason());
            false
        }
    }
}

fn check(cases: &[Case], table: &str) {
    if !require_sandbox() {
        return;
    }
    let mut failures = Vec::new();

    for case in cases {
        let ws = workspace();
        let outside = tempfile::tempdir().unwrap();
        let command = case
            .command
            .replace("{outside}", outside.path().to_str().unwrap());

        let (succeeded, output) = run(ws.path(), &command);

        // A containment case is judged on the host, not on the exit status:
        // the whole point is that succeeding is acceptable so long as nothing
        // escaped.
        if let Expect::ContainedFromHost { host_path } = case.expect {
            let escaped = ws.path().join(host_path);
            if escaped.exists() {
                failures.push(format!(
                    "  {table}: {}\n    reached the host at {}\n    because:  {}",
                    case.name,
                    escaped.display(),
                    case.why
                ));
            }
            continue;
        }

        let actual = if succeeded {
            Expect::Allowed
        } else {
            Expect::Denied
        };

        if actual != case.expect {
            failures.push(format!(
                "  {table}: {}\n    expected {:?}, got {:?}\n    command:  {command}\n    \
                 because:  {}\n    output:   {}",
                case.name,
                case.expect,
                actual,
                case.why,
                output.trim().replace('\n', " | ")
            ));
        }

        // A denial must also mean nothing landed. A command can "fail" while
        // having already done the damage, which is the failure mode an exit
        // code alone cannot distinguish.
        if case.expect == Expect::Denied {
            let leaked: Vec<_> = std::fs::read_dir(outside.path())
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            if !leaked.is_empty() {
                failures.push(format!(
                    "  {table}: {}\n    denied, but wrote outside anyway: {leaked:?}",
                    case.name
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "the permission contract was violated:\n\n{}\n",
        failures.join("\n\n")
    );
}

#[test]
fn the_permission_contract_holds() {
    check(CONTRACT, "contract");
}

#[test]
fn escape_attempts_are_contained() {
    check(ESCAPES, "escape");
}

// ---------------------------------------------------------------------------
// Invariants — properties that must hold for every case, not just the listed
// ones. These are what stop the table drifting into a list of examples.
// ---------------------------------------------------------------------------

/// Nothing in the contract may write outside the workspace, whatever its
/// expected outcome. Stated separately so a future `Allowed` row cannot quietly
/// introduce an escape by being written as a working case.
#[test]
fn no_contract_case_writes_outside_the_workspace() {
    if !require_sandbox() {
        return;
    }
    for case in CONTRACT.iter().chain(ESCAPES) {
        let ws = workspace();
        let outside = tempfile::tempdir().unwrap();
        let command = case
            .command
            .replace("{outside}", outside.path().to_str().unwrap());
        let _ = run(ws.path(), &command);

        let leaked: Vec<_> = std::fs::read_dir(outside.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            leaked.is_empty(),
            "`{}` wrote outside the workspace: {leaked:?}",
            case.name
        );
    }
}

/// `.git` must be intact after every case, including the ones expected to
/// succeed. A row added later that happens to clobber it should fail here even
/// if its own assertion passes.
#[test]
fn git_survives_every_case() {
    if !require_sandbox() {
        return;
    }
    for case in CONTRACT.iter().chain(ESCAPES) {
        let ws = workspace();
        let outside = tempfile::tempdir().unwrap();
        let command = case
            .command
            .replace("{outside}", outside.path().to_str().unwrap());
        let _ = run(ws.path(), &command);

        assert_eq!(
            std::fs::read_to_string(ws.path().join(".git/HEAD")).ok(),
            Some("ref: refs/heads/main\n".to_string()),
            "`{}` modified or removed .git/HEAD",
            case.name
        );
    }
}

/// The table has to keep covering the axes the model actually has. If a new
/// boundary appears, this is the reminder to describe it here rather than test
/// it only where it was implemented.
#[test]
fn the_contract_covers_every_boundary() {
    let names: Vec<&str> = CONTRACT.iter().chain(ESCAPES).map(|c| c.name).collect();
    for required in [
        "outside",   // the filesystem perimeter
        ".git",      // the recovery mechanism
        ".forge",    // the permission store
        "network",   // egress
        "symlink",   // indirection
        "workspace", // the working case
    ] {
        assert!(
            names.iter().any(|name| name.contains(required)),
            "no case mentions `{required}`; the contract has stopped covering a boundary"
        );
    }
}
