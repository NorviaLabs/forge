# forge-config

## Overview

Crate root for configuration. No source code at this level: all logic lives in
`src/`.

## Subdirectories & Boundaries

- `src/`: [./src/EGM.md](./src/EGM.md) — layered `forge.toml` loading with the untrusted-project boundary, permission rules, directory trust, themes, and pane-layout preferences.

## Notes

- This crate must not depend on session, tool, or UI crates; it only describes settings and persists user-global preference files.
- Trust decisions made here are consumed by `forge-session` (workspace trust gate) and `forge-cli` (launch/setup flow).

## Interface

- No symbols at this level. The crate surface (`Config`, `ConfigOverrides`, `ConfigError`, section structs, permissions/trust/theme/pane-layout APIs) is documented in [./src/EGM.md](./src/EGM.md).

## Invariants

- Layer order is defaults < user config < project `forge.toml` < env < CLI overrides; an auto-discovered project file is untrusted and cannot set `workspace_root`, `mcp.servers`, `model.base_url`, or `model.api_key`.
- Config files with a `version` newer than `CONFIG_SCHEMA_VERSION` are refused, never partially loaded.
- Workspace `.forge/permissions.toml` may only add deny rules; allow rules come from the user-global file.

## Side Effects

- Reads user/project TOML, env vars, and the process cwd at load time.
- Writes only user-global files under the platform config dir: `config.toml` (theme, smart-commit, pane layout), `trust.toml`, and `permissions.toml`.

## Verification

- Run: `cargo test --package forge-config --locked`
