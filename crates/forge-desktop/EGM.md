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
  - `DesktopEvent` (stream / session / attention / error / lagged), and
    `translate(SupervisorEvent)`.
  - Commands: `subscribe(Channel)`, `submit_prompt`, `stop_turn`.
  - It is replaced by the versioned `forge-app` protocol in phase P.
- `src/commands_list.rs`: `DESKTOP_COMMANDS`, the IPC allowlist shared by
  `build.rs` (`AppManifest::commands`) and the tests.
- `tauri.conf.json`: the release configuration (`dev.forge.desktop`).
  `tauri.self-test.conf.json` is identical except for its identity
  (`dev.forge.desktop.test`).
- `isolation/index.html`: the isolation-pattern hook (inline script).
- `capabilities/main.json`: the only capability. It is scoped to the
  `main` window and grants `core:event:allow-listen`/`unlisten` plus the
  allowlisted commands. There is no `core:default` and no plugins.
- `ui/`: the Svelte 5 + Vite frontend. Exact versions are pinned in
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
- The webview never receives credentials. Events carry session ids, turn
  states, message counts and streamed model output.
- K1 limits:
  - It needs a git repository (non-git arrives with track D).
  - It refuses an untrusted workspace with a message (the native trust
    prompt comes later).

## Verification

```sh
cd crates/forge-desktop/ui && pnpm install --frozen-lockfile && pnpm build
cd .. && cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo clippy --all-targets --locked --features self-test -- -D warnings
cargo test --locked && cargo test --locked --features self-test
cargo build --locked --features self-test && ./target/debug/forge-desktop --self-test
```
