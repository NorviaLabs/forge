# forge-desktop

## Overview

The Forge desktop app (Tauri 2 + Svelte). It runs the repository supervisor in-process and bridges it to
a webview over Tauri IPC. It opens no network listener.

This crate is its **own Cargo workspace**. The root workspace lists it
under `exclude`, so `forge-cli`, the TUI and `cargo clippy --workspace`
never depend on Tauri or WebKitGTK and still build on headless Linux. CI
guards this: the main job fails if `tauri`, `wry`, `tao` or `webkit2gtk`
appear in the root `Cargo.lock`.

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
    inspector) / roster (live sessions as `SessionRow`s) / selected (the
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
    and feedback, provider connect/disconnect/custom add-edit-remove, model
    switching, per-session PTYs, transcript detail (`message_detail`) and
    quit. Approvals always send `expected_call_id`; child approvals also send
    `request_id`. Prompt, stop, approval and workspace commands resolve
    their session from the supervisor's current selection, not from a copy
    in the page. Selecting publishes the id, then Rust sends that session's
    `Session` snapshot behind it.
  - It is replaced by the versioned `forge-app` protocol in phase P.
- `src/workspace.rs`: workspace browsing, search, file operations, image paste,
  the editor's save path and operator-driven Git writes.
  Confines every path to the active session's workspace, skips runtime and
  dependency directories, caps the tree and file/diff sizes, refuses binary
  files, and shells out to Git for status/diff/restore, stage/unstage, staged-only
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
  an environment variable for this process, as in the TUI.
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
  - Panes: session/file sidebar, conversation with streaming markdown and tool
    cards, an always-editable line-based text editor (files that cannot be
    saved back byte-for-byte open as a read-only viewer with the reason), a
    per-file diff with a restore action, inspector (model, branch, context,
    changes), and a lazily loaded xterm.js terminal. A model picker and a
    `⌘K` command palette are overlays; archive and unsaved-changes actions
    confirm in a native-style dialog.
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
  - The terminal emulator stays mounted per session across selection changes;
    closing its tab/session ends that shell. The real-webview self-test verifies
    both DOM identity and shell-variable persistence across a session switch.
  - Editor find/jump and lexical syntax coloring use text spans, never raw HTML;
    coloring is capped at 64 KiB. Reloading dirty text requires discard confirmation.
    The file/diff pane has no buttons: save/find/jump use shortcuts or the
    command palette, and reload/restore confirmations are app-level dialogs.
  - Image paste validates up to four 5 MiB images by bytes in Rust, rejects
    unsupported models and confines new attachment files to `.forge/attachments`.
    The page never supplies an attachment filesystem path.
  - Help and Forge Dark/Light are available from the sidebar; theme preference
    is local to the desktop and the PTY emulator follows it.
- Remaining limits:
  - It needs a git repository (non-git arrives with track D).
  - Native key collection is macOS-only, syntax colors are lexical, GitHub is
    read-only, and desktop OS notifications/proactive host editing remain absent.

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
