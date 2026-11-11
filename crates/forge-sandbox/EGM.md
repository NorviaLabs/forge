# forge-sandbox

## Overview

OS-level confinement for every process Forge starts. Depends on no other
workspace crate, so the crates at the bottom of the graph (`forge-storage`,
`forge-workspace`) can confine the git they run. `forge-tools` re-exports it
as `forge_tools::sandbox` and adds the egress-proxy glue.

## Interface

- `SandboxPolicy` (`for_workspace`, `with_home`, `with_session_tmp`,
  `with_tool_cache`, `default_tool_cache`, `with_read_path`,
  `with_write_path`, `with_loopback_port`, `with_read_only_workspace`, `with_git_writable`,
  `with_git_open`, `with_command_access`, `with_egress_proxy`,
  `with_egress_socket`, `toolchain_env`), `seatbelt_profile`,
  `wrap_shell_command`, `wrap_command`, `availability`, `Unavailable`,
  `temp_env`, `shell_env`, `egress_env`.
- `git::command_networked(repo, also)`: confined git with the network the
  installed `git::NetworkProvider` supplies for that repository (gateway
  port, SSH and signing broker ports, environment), plus write access to
  remotes that are repositories on disk (`git::local_remote_repositories`).
  With no network it is still confined, with none. Only a host that cannot
  confine gets `git::unconfined`. `git::signing_configured(repo)`: such a
  commit must use `command_networked`.
- `git::command_checking_out(repo, also)`: for switching branch and
  `worktree add`; `command_writing`, or `command_networked` when
  `git::uses_lfs(repo)`.
- `git::command(repo)` / `git::command_writing(repo, also)`: `git -C <repo>`
  confined to the worktree containing `repo` and its git dirs, whole git dir
  writable, no network. `git::unconfined(repo)`: host git, only where the
  host cannot confine.

## Invariants

- Reads: whole disk except the deny-list; the session's own trees are
  reopened after the denies and deny-list entries inside them are denied
  again. Nothing derived from command text changes the profile.
- Writes: workspace, session temp directory, tool cache, and paths named
  with `with_write_path` (which hold even inside the `.forge` carve-out).
- A linked worktree's `.git` pointer is believed only when the git dir names
  the worktree back (`<common>/worktrees/<name>/gitdir`, or `core.worktree`
  for a submodule). The common dir comes from the layout, never from the
  `commondir` file.
- Every call of `git::unconfined` is a place a hook or config-named program
  still runs outside the sandbox; the agent's policy therefore keeps
  `.git/hooks` and `.git/config` read-only (`with_git_writable`, not
  `with_git_open`).
- A host that cannot confine gets plain `git` from `git::command`.

## Side Effects

- Reads the filesystem to resolve paths; creates the tool cache directory
  and, in `toolchain_env`, cache subdirectories, a cargo config symlink and
  a sanitized npmrc. Spawns nothing itself.

## Verification

- Run: `cargo test --package forge-sandbox --locked`
