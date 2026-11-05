# forge-desktop

## Overview

The Forge desktop app (Tauri 2 + Svelte), at the walking-skeleton stage
(phase K1). It runs the repository supervisor in-process and bridges it to
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
    `MessageView`s, and `pending_approval` as an `ApprovalView` with
    invisible/bidi characters escaped by `escape_invisible`) / attention /
    error / lagged.
  - Commands: `subscribe(Channel)`, `submit_prompt`, `stop_turn`,
    `approve_once(call_id)`, `deny(call_id)`. Approvals always send
    `expected_call_id`.
  - It is replaced by the versioned `forge-app` protocol in phase P.
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
- K1 limits:
  - It needs a git repository (non-git arrives with track D).
  - It refuses an untrusted workspace with a message (the native trust
    prompt comes later).

## Verification

```sh
cd crates/forge-desktop/ui && pnpm install --frozen-lockfile && pnpm build && pnpm build:self-test
cd .. && cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo clippy --all-targets --locked --features self-test -- -D warnings
cargo test --locked && cargo test --locked --features self-test
cargo build --locked --features self-test && ./target/debug/forge-desktop --self-test
```
