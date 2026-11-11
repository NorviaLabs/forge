# forge-desktop

## Overview

The Forge desktop app (Tauri 2 + Svelte). It runs the repository supervisor in-process and bridges it to
a webview over Tauri IPC. It opens no network listener.

This crate is a member of the root workspace, so a plain `cargo build` builds
it. `build.rs` builds the web UI (`ui/`, Vite) whenever its sources change,
installing `ui/node_modules` with `pnpm install --frozen-lockfile` when absent;
`FORGE_SKIP_UI_BUILD=1` uses a prebuilt `ui/dist` instead. On Linux the
workspace build needs the WebKitGTK/GTK development packages. The built-in
themes are `themes/forge-dark.toml` and `themes/forge-light.toml`.

## Layout

- `src/main.rs`: entry point.
- `src/startup.rs`: arguments (`forge-desktop [WORKSPACE]`; `--self-test`
  only in `self-test` builds), then `forge_session::LaunchPlan` (read-only
  ownership probe, trust check, launch), then the supervisor and the Tauri
  event loop. On exit it runs `RepositorySupervisor::shutdown`, bounded at
  10 s.
- `src/bridge.rs`: the provisional IPC bridge.
  - `DesktopEvent`: stream / session (turn state, the transcript as
    `MessageView`s, `pending_approval` as an `ApprovalView` with
    invisible/bidi characters escaped by `escape_invisible`, and the
    workspace, branch, model, context usage and queue length for the
    Session menu) / roster (live sessions as `SessionRow`s) / selected (the
    session commands target) / attention / error / lagged.
  - Commands: `src/commands_list.rs` is the authoritative allowlist; the
    families are `subscribe`/events, prompt submission (text, pasted images
    and an optional open-file context block), turn stop/continue, approvals
    and questions, session create/select/fork/resume/close/archive/rename/
    pin/stop, worktree attach and managed-worktree removal (archive first;
    a dirty checkout returns retained until the operator opts into
    discarding), queued-prompt edit/cancel/recovery, child transcripts and
    background tasks (cancellation is scoped to one run, so a stale click
    cannot stop a replacement), workspace tree/read/write/search/file
    operations and image reading,
    typed Git verbs (stage/unstage/commit/branch/remote/merge abort/hunk
    discard), GitHub issue/PR/check views plus explicit PR creation, refresh
    and feedback, provider connect/disconnect/custom add-edit-remove,
    installed theme listing, model switching, per-session PTYs, transcript
    detail (`message_detail`) and
    quit. Approvals always send `expected_call_id`; child approvals also send
    `request_id`. Prompt, stop, approval and workspace commands resolve
    their session from the supervisor's current selection, not from a copy
    in the page. Selecting publishes the id, then Rust sends that session's
    `Session` snapshot behind it.
  - It is permanent (no versioned protocol replaces it). Projects: one
    `ProjectService` holds every open project. `DesktopState.projects` is its
    handle and `active` the project on screen. A command with `session_id`
    resolves its project with `project_for`, which is
    `ProjectServiceHandle::project_of` (session targeting: the open project
    that lists the session live or holds its durable row; a session no open
    project knows is an error, never the project on screen); every other
    command uses `active()`. Reads go through `OpenProjectRef`'s methods
    (`snapshot`, `snapshots`, `session_records`, `selected_session_id`,
    `journal_dir`); the supervisor lock is private to `forge-session`.
    A command that takes `session_id` returns `SessionFailure { kind,
    message }`: `unknown_session` (no open project knows the id; the page
    forgets the row), `shutting_down` (the project closed underneath it; the
    page stays quiet) or `error`. The page calls the backend only through
    `ui/src/ipc.js`, which turns a `{ kind, message }` rejection into a
    `CommandFailure` (`ui/src/failure.js`) that still prints as its message. Provider changes reload every open project. `subscribe`
    follows the on-screen project (a switch sends `ProjectSwitched`, then the
    roster, selection and session) and forwards `Workspace` (every project's
    sessions, pending requests and run slots) and `AttentionNew { count,
    sound }`. The badge is `set_badge_count(total pending)`; new requests ask
    the OS for attention only when the window isn't focused, and `sound` is
    false when they are all in the session on screen with the window
    focused. Project commands: `project_switch` (opens if closed),
    `project_add` and `project_relocate` (folder picker), `project_trust`,
    `project_close` (refuses the on-screen project; `busy` lists sessions by
    label), `project_remove`; failures are `ProjectFailure { kind, message,
    sessions }`. Startup resolves the launch folder through the registry: a
    registered project or Git repository opens; an unregistered plain folder
    waits on the trust screen as `Pending::Confirm` (accepting adds, trusts
    and opens it).
  - Page (4c): a Projects panel at the top of the sidebar lists every
    registered project at once (capped at 40% height, scrolls): a dot per
    project (`view.js` `projectStatus`: ring = closed, muted = idle, accent =
    working, warning = waiting), a count badge for pending requests or
    `running/limit`, and hover actions (Close for open background projects,
    Remove for closed ones). The `+` adds a project. A background project's
    waiting requests are listed under its row; selecting one switches
    project, then opens the session at its decision card. The command
    palette has a Projects group (switch to each, Add a project…).
    `needs_trust` and `root_missing` failures open a dialog (Trust and open / Locate… / Remove); `busy` opens the close
    dialog (Go to session / Stop all and close). Session view records and
    shells are keyed by session id and kept across switches: a roster prunes
    only sessions owned by the project on screen (`view.js` `prunable`). The
    chime (`chime.js`) is Web Audio, unlocked on the first gesture, and
    played for `AttentionNew { sound: true }` when
    `localStorage["forge.desktop.attentionSound"]` isn't `off` (toggle in
    Help). The trust screen says "Open this folder as a project?" for
    `needs_confirmation`.
  - Launch with no folder (Dock, Finder): the folder screen lists registered
    projects (`list_projects`) and the page reopens
    `localStorage["forge.desktop.lastProject"]` with `open_project`. An
    untrusted project moves to the trust screen; any other failure (moved,
    owned elsewhere) keeps the folder screen with the reason. Picking a
    listed project uses the same command.
  - Quitting asks first when any open project, not just the one on screen,
    has a queued, running or waiting session (`quit_risk` reads
    `WorkspaceState::has_busy_sessions`); the quit dialog names the
    background projects (`ProjectView.busy`). Errors and failed turns in
    projects not on screen reach the page as `ProjectError { project_id,
    project_name, message }` (`fresh_project_errors`, once per `seq`; none
    replayed at startup or on a switch). `ProjectFailure` carries
    `project_id` and its message starts with the project name.
- Files tree: lazy, one folder per request. `list_dir(session_id, path)`
  (`""` = root) returns that folder's children (directories first) as
  `DirListing { entries: [{ path, name, is_dir, ignored }], truncated }`; only
  a single folder above 20,000 entries is cut (`truncated`). `ignored` comes
  from `.gitignore` (the folder's and every parent's, `.git/info/exclude`, the
  global excludes file; honoured outside Git too) plus conventional build and
  dependency folders (`node_modules`, `target`, `dist`, `.zig-cache`,
  `zig-out`, `.venv`, …). Ignored entries are listed and hidden by the page
  until "Show ignored files". `.git` and `.forge` are never listed and cannot
  be browsed. The page keeps loaded folders in `listings`, open folders in the
  session's view record (`expanded`), flattens them with `view.js`
  `flattenTree`, and renders only the rows on screen (`windowRange`, 24 px
  rows). A refresh reloads the root and the visible open folders
  (`foldersToLoad`); a folder that is gone is closed.
- Search: `find_files` (Quick Open by fuzzy name, 50 results) and
  `search_files` (content, 200 hits; plain text folded to lower case, `/…/`
  is a regex) go through one watched `forge_search::WorkspaceIndex` per
  workspace root (`workspace::Indexes`), opened on the first search there. No
  file-count limit; the index is gitignore-aware and does not follow links
  out of the workspace. The command palette and the Files filter query it as
  you type, so files in folders never opened are found.
- `src/workspace.rs`: workspace browsing, search, file operations, image paste,
  the editor's save path and operator-driven Git writes.
  Confines every path to the active session's workspace, caps file/diff
  sizes, refuses binary files, and shells out to Git for status/diff/restore, stage/unstage, staged-only
  commits and branch operations. `read` returns a SHA-256 fingerprint, LF-normalized
  text and whether the file used CRLF; files above the size cap or not valid
  UTF-8 are marked read-only. `read_image` returns a magic-byte-checked
  5 MiB-capped data URL, so the page can show a file or a transcript
  attachment without filesystem access. `write` refuses a fingerprint that no longer
  matches (an agent may have edited the file), restores CRLF, and saves
  through a sibling temp file so an interrupted save leaves the original.
  A close request with unsaved buffers is held by the run loop, which emits
  `close_requested`; `quit_app` ends the process only after the user
  confirms.
- `src/shell.rs`: one interactive PTY per session. `open` spawns `$SHELL
  -il` in the session workspace, streams base64 output to the page's channel,
  and accepts keystrokes and resize events. Closing kills the child. No agent
  tools, sandbox, or approvals are involved.
- `src/providers.rs`: existing `forge-connect` services for connect/disconnect,
  model catalog refresh, adopting a sign-in another CLI already completed
  (Codex, OpenCode, Grok) and native macOS hidden-input key collection. OAuth
  tokens/device secrets stay in Rust; only the sign-in URL/user code and the
  imported source/account label cross IPC. All provider paths share
  `Credentials::shared()`, so a key typed in the native prompt also wins over
  an environment variable for this process.
  Missing keys on other platforms require environment variables or terminal setup.
- `src/github.rs`: issue/PR/check views using `forge-workspace`'s bounded `gh`
  adapter, plus explicit PR creation, refresh and review-feedback application.
  CI logs are capped at 64 KiB. No merge, force-push or branch-deletion verbs.


- `src/self_test.rs` (`self-test` feature only):
  - the page's report arrives via navigation to `/?selftest=<json>`, so
    there is no extra IPC command;
  - Rust adds its own checks: the prompt was submitted, a completed turn was
    delivered, no event emit got through, the *embedded* config matches the
    pinned values and the test identity, and the only TCP listener is the
    loopback egress proxy (via `lsof`).
- `src/pinned.rs`: `PINNED_CSP`, shared by the self-test and the config
  tests.
- `src/commands_list.rs`: `DESKTOP_COMMANDS`, the IPC allowlist shared by
  `build.rs` (`AppManifest::commands`) and the tests.
- `tauri.conf.json`: the release configuration (`dev.forge.desktop`).
  `tauri.self-test.conf.json` is identical except for its identity
  (`dev.forge.desktop.test`).

  **Gotcha:** self-test builds apply `tauri.self-test.patch.json` through
  `TAURI_CONFIG`, which build.rs sets for tauri-build and emits as
  `rustc-env` for `generate_context!`. Passing another config *file* to
  `generate_context!("…")` silently does nothing: it only selects the
  directory and still reads `tauri.conf.json`.
- `isolation/index.html`: the isolation-pattern hook (inline script).
- `capabilities/main.json`: the only capability. It is scoped to the
  `main` window and grants `core:event:allow-listen`/`unlisten` plus the
  allowlisted commands. There is no `core:default` and no plugins.
- `ui/`: the Svelte 5 + Vite frontend.
  - Screens: a session rail (project, Needs you, session search, the roster
    in order, New session and Help; an overlay at compact width) beside one
    center column. The primary session screen is the conversation with
    streaming markdown and tool cards, the current plan step, a queue strip
    only while prompts are queued, the decision cards, and the composer with
    its terminal toggle and context meter. Files and Terminal are focus
    screens entered from the session header and left with `‹ Session` or
    Esc. Files holds the tree, scopes (All, Changed, Staged, Last turn), row
    staging, Stage all, commit, upstream controls and the document tabs over
    an always-editable line-based text editor (files that cannot be saved
    back byte-for-byte open as a read-only viewer with the reason) and
    per-file diffs. The Session menu is a modal for plan, agents and tasks,
    queue, context and usage, the model and session actions. A model picker
    and a `⌘K` command palette are overlays; archive and unsaved-changes
    actions confirm in a native-style dialog.
  - View state: `src/view.js` keeps one record per session (screen, drawer,
    open documents, active document, file scope and filter, draft,
    transcript scroll). Selection only changes which record renders; there
    is no park or restore step, and document loads that finish after a
    switch land in the record of the session that started them.
  - Model text renders through `src/markdown.js`: markdown-it with
    `html: false` and links, images and autolinks disabled, then DOMPurify.
    It reaches the DOM only in `Markdown.svelte`, the single raw-HTML site
    (enforced by a test).
  - `pnpm build` produces `dist/`; `pnpm build:self-test` produces
    `dist-self-test/`, which alone contains `src/selftest.js`. Exact versions are pinned in
  `package.json` and `pnpm-lock.yaml`. It builds into `ui/dist`, which the
  binary embeds, so build the UI before `cargo build`.
- `tests/config.rs`: the security configuration checked as data, including
  the ACL that tauri-build generates.

## Invariants

- Streaming: the bridge sends tokens (`Stream`, text deltas only) and full `Session` snapshots for the session on screen only; another session's update is a `SessionState { session_id, turn_state }`. Text deltas already queued are joined into one `Stream` (up to 16 KB). The page applies deltas once per animation frame and renders the streamed message as blocks (`view.js` `streamSegments`, split at blank lines outside code fences), so only the growing tail re-renders. `FORGE_LOG` (an `EnvFilter`, e.g. `forge::timing=debug`) turns on stderr logging, including `forge::timing` lines for model-call return and turn publication; the webview logs streamed updates slower than 16 ms with `console.debug`.

- The webview can call only the commands in `DESKTOP_COMMANDS`, and only
  from the `main` window. Self-test builds expose exactly the same
  commands.
- The CSP is pinned (`tests/config.rs::PINNED_CSP`). `'unsafe-inline'` is
  allowed for styles only. Scripts, frames, objects and every remote origin
  are closed.
- `withGlobalTauri` is off and `freezePrototype` is on.
- The **isolation pattern** is on. `isolation/index.html` holds the hook
  *inline*, because Tauri serves only `index.html` from that directory. The
  hook passes exactly `DESKTOP_COMMANDS` plus event listen/unlisten and drops
  everything else. The CSP has no `frame-src`: Tauri appends its per-build
  isolation scheme (`isolation-<uuid>:`) to `default-src`, and an explicit
  `frame-src` would override that and block the isolation frame.
- Navigation is limited to the bundled app (`tauri://localhost` /
  `http(s)://tauri.localhost`) by a Rust-only `on_navigation` plugin. It
  exposes no commands.
- The webview never receives credentials. Events carry session ids, turn
  states, message counts and streamed model output.
- Parity surfaces (see `PARITY.md` for explicit remaining limits):
  - Question cards, typed approvals and denial notes, approve-all/clear rules,
    session rename/pin/fork/resume/close/continue, plan, token/context breakdown,
    provider usage/cost and tool/skill detail, background tasks and child approvals.
  - Privileged approval grants require a second in-page confirmation and Rust
    checks it; this is not an OS-native confirmation boundary.
  - `trust_status` precedes subscription. An untrusted folder opens a trust
    screen; no supervisor/session exists until explicit consent. `trust_workspace`
    grants trust through `forge-config`, prepares again and launches in-place.
  - Durable and actor queues retain their source and stable ID through IPC;
    cancelling a stale row refuses rather than removing its successor.
  - Each session has one terminal host, mounted from the first open until its
    shell exits, it is stopped, or the session closes; a class shows it as the
    drawer under the composer, the focused Terminal screen, or hidden. The
    real-webview self-test verifies one host element across drawer, focus and
    a session switch, plus shell-variable persistence.
  - Editor find/jump and lexical syntax coloring use text spans, never raw HTML;
    coloring is capped at 64 KiB. Reloading dirty text requires discard confirmation.
    The editor body has no buttons: save/find/jump use shortcuts or the
    command palette, and reload/restore confirmations are app-level dialogs.
    Staging, commit and upstream controls live in the Files screen around it.
  - Image paste validates up to four 5 MiB images by bytes in Rust, rejects
    unsupported models and confines new attachment files to `.forge/attachments`.
    The page never supplies an attachment filesystem path.
  - An `Attention` event for an unfocused window calls the core window API
    (`request_user_attention`), so the dock/taskbar asks for the user without
    adding a notification plugin. The event still carries the session and
    message for the in-app badge and title.
  - Help and the theme picker are available from the rail. `list_themes`
    returns the built-ins plus `~/.config/forge/themes` and workspace
    `.forge/themes` as CSS-token palettes, so a drop-in theme works in both
    frontends; `system` follows the OS preference in the page. The preference
    is local to the desktop and the PTY emulator follows the resolved palette.
- Remaining limits:
  - A folder that isn't a Git repository opens under the lease; `Hello.supports_git` is false and Git-only actions stay visible but disabled with the reason: the palette's Git group, attach worktree and GitHub; the Files pane's branch chip (shown as "No Git"), Fetch/Pull/Push, and the Changed/Staged/Last-turn scopes. `refreshFiles` skips `git_status`/`git_last_turn_files` there, so the file list still loads. The trust screen adds "No Git: agent edits can't be rolled back." (`TrustView.no_git`, detected only while trust is pending). Config is loaded with `project_root` set to the workspace.
  - A launch with no path from `/` or the home folder (Dock, Finder) opens nothing: `Startup::NeedsFolder` → `TrustState::needs_folder()` → `TrustView.needs_folder`, and the page shows "Choose a folder…". The `choose_folder` command opens a native picker (`rfd`, created on the main thread; deliberately not the Tauri dialog plugin, which would add webview-reachable IPC), then loads config, prepares the launch, and either moves to the trust screen or opens the session. Cancelling keeps the picker. `choose_folder` is in the command allowlist, `capabilities/main.json` and the isolation hook.
  - Native key collection is macOS-only, syntax colors are lexical, GitHub is
    read-only, notification-centre banners and proactive host editing remain absent.

- `main` intercepts `forge_tools::egress_connect::SUBCOMMAND` before
  `startup::run`: git's SSH routing runs this executable as the
  ProxyCommand, so without it every SSH push or fetch from a desktop
  session failed with "unexpected argument".

## Verification

```sh
cd crates/forge-desktop/ui && pnpm install --frozen-lockfile && pnpm build && pnpm build:self-test
cd .. && cargo fmt --check
cd ui && node --test src/editor.test.js src/modal.test.js
cd ..
cargo clippy --all-targets --locked -- -D warnings
cargo clippy --all-targets --locked --features self-test -- -D warnings
cargo test --locked && cargo test --locked --features self-test
cargo build --locked --features self-test && ./target/debug/forge-desktop --self-test
```
