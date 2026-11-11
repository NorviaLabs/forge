# forge-config source

## Overview

Configuration loading (defaults → user TOML → project `forge.toml` → env →
CLI overrides) with a trust boundary on the project layer, plus the three
file-backed preferences this workspace owns: permission rules, directory
trust, and theme/pane-layout state.

## Interface

All submodules are private; the crate surface is `lib.rs` plus its `pub use` re-exports of the items below.

`lib.rs`:

- `CONFIG_SCHEMA_VERSION: u32 = 1`.
- `ConfigError` — `Io`, `Toml`, `InvalidProvider` (declared but never constructed), `Message`, `UnsupportedSchemaVersion { path, found, supported }`.
- Sections with `Default`: `ModelConfig { request_timeout_secs (300), mock (serde-skipped; set only in code to use the offline mock client), mock_script (only with the `fake-provider` feature; serde-skipped) }`. With that feature, `Config::load` calls `apply_fake_provider`: a non-empty `FORGE_FAKE_PROVIDER_SCRIPT` sets `mock = true` and `mock_script`. Builds without the feature never read the variable, `JournalConfig { backend: "sqlite", path }`, `McpServerConfig`, `McpSection`, `CommandConfig`, `ValidationConfig`, `ToolsConfig`, `AgentConfig`.
- `default_journal_path() -> ".forge/sessions"`, `expand_sandbox_read_path(raw, home) -> Result<PathBuf, String>`.
- Enums: `McpSandboxWorkspace{ReadOnly (default), ReadWrite}`.
- `Config { workspace_root, model, journal, mcp, validation, tools, resolved_workspace (skip), refused_project_keys (skip), removed_model_keys (skip) }` with `workspace_root()`, `journal_dir()`, `refused_key_notices()` (also one notice per `removed_model_keys` entry, pointing at `/connect`), `load(ConfigOverrides) -> Result<Self, ConfigError>`.
- `ConfigOverrides { config_path, workspace, journal_path, project_root }`. `project_root`, when set, replaces the process working directory for `forge.toml` discovery and as the base for resolving the workspace, so a process can load config for a project other than its launch folder; an unreadable `project_root` is `ConfigError::Io` (never a fallback). The project layer stays untrusted. Environment variables apply to every project; the launch-only overrides (`config_path`, `workspace`, `journal_path`) apply to the project they were given for.
- `[model] provider/model/base_url/api_key` are no longer read: a file that sets them still loads, and each key is recorded in `removed_model_keys`. Provider and model are chosen with `/connect`; keys and endpoints live in `credentials.toml` and `providers.toml`.

`permissions.rs`: `PermissionsFile { allow, deny }`, `parse_permissions_toml`, `user_permissions_path`, `workspace_permissions_path(workspace)`, `load_permissions(workspace) -> (PermissionsFile, Vec<String>)`, `append_user_allow_rule`.

`trust.rs`: `TrustError`, `trust_file_path`, `is_trusted`, `is_trusted_at`, `grant_trust`, `grant_trust_at`.

`theme.rs`: `Rgb` (+`to_hsl`), `ThemePalette`, `SyntaxPalette`, `ThemeDefinition`, `parse_theme_toml` (the desktop's theme files; the built-ins live in `crates/forge-desktop/themes/`).

## Invariants

- Load order is fixed: defaults < user XDG config < project `forge.toml` < env < CLI overrides. `resolved_workspace` is computed last from `workspace_root`, `--workspace`, and the process cwd.
- The project layer from an auto-discovered `./forge.toml` is **untrusted**; a path named explicitly via `--config` is trusted. Untrusted layer refuses `workspace_root`, `journal`, and a non-empty `mcp.servers`, recording each in `refused_project_keys` so the caller can surface a notice instead of a silent behaviour change. `[model] provider/model/base_url/api_key` are ignored from every layer and recorded in `removed_model_keys` instead.
- A file whose `version` exceeds `CONFIG_SCHEMA_VERSION` is refused, never partially loaded; a file with no `version` reads as version 1.
- Permission scopes are not equally trusted: personal file `allow` + `deny` are merged; workspace `.forge/permissions.toml` contributes **deny only** — its `allow` entries are ignored and a diagnostic is emitted, because they would skip approvals.
- `load_permissions` never fails: malformed files produce diagnostics alongside the merged rules; a missing or unreadable file is skipped silently.
- Trust is inherited: a path is trusted when it or an ancestor is listed in the user-global `trust.toml`. A canonicalization failure means "not trusted". An unparseable trust file reads as untrusted.
- Env layer variables: `FORGE_WORKSPACE`, `FORGE_JOURNAL_PATH`, and `FORGE_MODEL_REQUEST_TIMEOUT_SECS`; an unparseable timeout is ignored. A legacy `[tools.web_search]` section is accepted and ignored.

## Side Effects

- Reads `$XDG_CONFIG_HOME/forge/config.toml` (or platform equivalent), `./forge.toml`, env vars, and the process cwd.
- Reads `forge/permissions.toml` (user) and `<workspace>/.forge/permissions.toml`.
- Writes: user `trust.toml` (trust grants), and user `forge/permissions.toml` (`append_user_allow_rule`). All user files live under the platform config dir (`dirs::config_dir()/forge/`).
- Writers never replace a file they cannot parse: `append_user_allow_rule` returns `InvalidData` on a malformed `permissions.toml` (and the read error on an unreadable one); `grant_trust` returns the parse error on a malformed `trust.toml`. Each leaves the file unchanged.
- `Config::default()` performs `env::current_dir()`.

## Verification

- Run: `cargo test --package forge-config --locked`

- `project_hosts(workspace)` / `append_project_host(workspace, host)`: hosts allowed "always" for a project, stored in `<config dir>/forge/project-hosts.toml` keyed by the project path (a managed worktree under `.forge/local/worktrees/` maps to its project). `load_permissions` merges them as `host(...)` allows.
