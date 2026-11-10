# AGENTS.md

Guidance for coding agents working in this repository.

## Project Overview

Forge is a Rust workspace for an AI coding-agent harness with a desktop app. `cargo build` builds both binaries: `forge-desktop` (the Tauri app, `crates/forge-desktop`) and `forge` (`crates/forge-cli`, which opens the app and runs `forge bench`). The desktop crate's build script builds its web UI (`crates/forge-desktop/ui`, Svelte, pnpm); Linux also needs the WebKitGTK/GTK development packages.

Key crates:

- `crates/forge-desktop`: Tauri desktop app: IPC bridge (`src/bridge.rs`), startup, and the Svelte UI in `ui/`.
- `crates/forge-cli`: `forge` command: opens the desktop app, `forge bench` headless runs, `__egress-connect` helper.
- `crates/forge-session`: Session assembly, the repository supervisor, the project registry and `ProjectService`.
- `crates/forge-core`: Agent loop, message/session lifecycle, tool execution orchestration.
- `crates/forge-model`: Native provider transports and wire-format normalization.
- `crates/forge-connect`: Provider profiles, credential store, model catalog, `/connect` support.
- `crates/forge-tools`: Built-in tools and validation.
  Includes `update_plan` (model-callable checklist; emits a `plan_update`
  turn event; no persisted plan state) and `ask_user_question` (pauses the
  turn on `WaitReason::Question` until the user answers).
- `crates/forge-mcp`: MCP client and remote tool registration.
- `crates/forge-config`: Config loading and model/provider migration.
- `docs/`: User/design documentation.

## Development Rules

- Keep changes focused; avoid unrelated refactors, dependency updates, or formatting churn.
- Prefer small root-cause fixes over call-site patches.
- Match existing Rust style and crate-local patterns.
- Always run relevant tests for the code you change before handoff.
- Add or update tests for behavior changes, and implement new test cases when changes are significant.
- Update docs when commands, configuration, provider behavior, architecture, or safety behavior changes.
- Never commit API keys, OAuth tokens, credentials, `.forge/` runtime data, or proprietary fixtures.

## Validation

Run only the tests that target the code you changed. Never run the full
workspace test suite (`cargo test --workspace`); it is slow and agents should
not spend time on it.

- For any non-trivial code change, run the most relevant crate-level tests,
  filtered to the modules/behaviors you touched where possible.
- For significant changes, add or expand targeted test coverage and run those
  new tests plus the directly affected ones.

```sh
cargo test --package forge-desktop --locked <filter>
cargo test --package forge-core --locked <filter>
cargo test --package forge-session --locked <filter>
```

Desktop UI (Svelte) tests:

```sh
(cd crates/forge-desktop/ui && node --test src/*.test.js)
```

Static checks:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

For CLI/release-sensitive changes:

```sh
cargo build --release --locked --package forge-cli
./target/release/forge --version
```

## Git Workflow

- Never add any document from docs/ folder to git.

## Provider/Model Notes

- Native model transports are in `crates/forge-model/src/native/`.
- Provider connection profiles and catalog handling are in `crates/forge-connect/src/`.
- OpenAI-compatible message/tool normalization is in `crates/forge-model/src/normalize.rs`.
- Be careful with provider-specific wire quirks; preserve compatibility with existing tests.

## Safety Notes

- Tool argument validation must happen before execution.
- File writes should stay workspace-confined.
- Durable/session changes should preserve resume behavior.
- When testing Forge against live repositories, use disposable worktrees or committed/backed-up work.
