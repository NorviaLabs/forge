# forge-tools — Tool Protocol Implementation

## Overview

Implements the schema-validated tool protocol (`Tool` trait + `ToolRegistry`)
and every built-in tool. This crate is the trust boundary between model-authored
JSON and the machine: it validates arguments before dispatch, confines file
paths to the workspace (plus an optional session scratch directory), confines
shell processes through OS sandboxes, strips provider credentials from child
processes, and routes network access through a domain-filtered egress proxy.

Subdirectories:

- [web_search](./web_search/EGM.md) — `web_search` tool and backend extension
  point.

## Interface

- Core protocol (`lib.rs`):
  - `Tool` trait — `name`, `description`, `input_schema`,
    `side_effect_class`, `idempotent` (default `false`), `parallel_safe`
    (default `false`), `async call`, `shutdown`, `cancel_session`,
    `descriptor`, `warm_workspace`.
  - `ToolError` — `Validation`, `Unknown`, `Execution`, `SandboxDenied`,
    `Io`; `as_outcome()` maps to `ExecutionOutcome`.
- Registry (`registry.rs`):
  - `ToolRegistry` — `new`, `register`, `install_default_builtins`, `get`,
    `list_descriptors`, `names`, `len`, `is_empty`, `name_classes`,
    `call`, `prepare_call`, `call_prepared`, `validate_call`, `shutdown`,
    `cancel_session`, `warm_workspace_index`.
  - `ToolContext` — workspace root, session id, principal, image-input gate,
    active model, optional `EgressGrant`, optional `SessionTempDir`,
    `unconfined_shell`, `tool_call_id`, `credential_env_names`; constructor
    `new(workspace_root)`; builders `with_credential_env_names`,
    `with_tool_call_id`, `with_session_tmp`, `with_session_id`,
    `with_unconfined_shell`; `gateway_for_call`, `resolve_path`,
    `resolve_write_path`.
  - `SessionTempDir` — per-session temporary directory (0700 on Unix),
    `create`, `path`.
  - `canonical_tool_name`, `canonicalize_tool_call`, `ValidatedToolCall`.
- Built-ins (`builtins.rs`): `ReadFileTool` (`read_file`), `LsTool` (`ls`),
  `WriteFileTool` (`write_file`), `BashTool` (`bash`), `GitTool` (`git`),
  `UpdatePlanTool` (`update_plan`), `AskUserQuestionTool`
  (`ask_user_question`), `default_builtins`,
  `default_builtins_with_web_search`, `run_shell_command`,
  `run_shell_command_observed`, `run_shell_command_with_egress`,
  `run_shell_command_with_egress_and_temp`, `run_shell_command_unconfined`,
  `ShellOutputCapture` (`snapshot`), `ShellOutputSnapshot`,
  `ProcessGroupGuard` (`new`, `kill`, `disarm`),
  `set_process_group`, `PROVIDER_CREDENTIAL_ENV`, `is_provider_credential_env`,
  `provider_credential_env_names`, `strip_credentials`. The
  `background_run` and `request_unconfined_retry` tools and every built-in's
  argument struct are crate-internal (registered via `default_builtins`, not
  re-exported).
- Editing: `EditTool` (`edit`) with `EditArgs`; `ApplyPatchTool`
  (`apply_patch`) with `ApplyPatchArgs`.
- Search/index (crate-internal, `fast_file_tools.rs`): `fff_tools` builds
  the `glob`, `grep`, and `edit` tools over a shared `forge-search` index;
  none of it is re-exported.
- Execution: `unified_exec_tools() -> (ExecCommandTool, WriteStdinTool)`
  (`exec_command`, `write_stdin`); the argument structs are crate-internal.
- Agent orchestration tools (`orchestration.rs`): `SpawnAgentTool`,
  `SendMessageTool`, `FollowupTaskTool`, `WaitAgentTool`, `ListAgentsTool`,
  `InterruptAgentTool`, with `SpawnAgentArgs`, `AgentMode`, `AgentMessageArgs`,
  `WaitAgentArgs`, `ListAgentsArgs`, `TargetAgentArgs`.
- Skills: `LoadSkillTool` (`load_skill`), `LoadSkillArgs`.
- Media/fetch: `ViewImageTool` (`view_image`), `ViewImageArgs`;
  `WebFetchTool` (`web_fetch`), `WebFetchArgs`, `web_fetch_tool`.
- Validation: `validate_args`, `validation_error_signature`,
  `ValidationBudget` (`new`,
  `with_default_max`, `record_failure`, `record_failure_with_signature`,
  `reset_turn`).
- Output shaping: `compress_tool_output`.
- Invocation display: `tool_invocation`.
- Sandbox (`sandbox.rs`): `SandboxPolicy` (`for_workspace`,
  `with_read_only_workspace`, `workspace_writable`, `with_command_access`,
  `toolchain_env`, `with_read_path`, `with_git_writable`, `with_session_tmp`,
  `with_egress_proxy`, `egress_proxy_port`, `with_egress_socket`,
  `egress_socket`, `workspace_root`, `readonly_subpaths`, `with_egress`),
  `Unavailable`
  (`reason`, `startup_message`), `availability`,
  `seatbelt_profile`, `wrap_shell_command`, `wrap_command`, `shell_env`,
  `temp_env`, `egress_env`, `explain_denial`, `EgressGrant`
  (`take_denied_host`, `permits_host`, `allow_patterns`),
  `SANDBOX_PROXY_PORT`.
- Shell spawn (`shell_spawn.rs`): `ShellSpawnMode`, `ShellSpawnRequest`,
  `ShellSpawnSpec` (`build`, `confined`, `program`, `args`, `env_remove`,
  `env_set`, `egress_invocation`, `take_egress_invocation`, `tokio_command`,
  `pty_command`).
- Egress (`egress.rs`): `EgressPolicy`, `EgressOptions`, `EgressOutcome`,
  `EgressDenial`, `EgressEvent`, `EgressShared`, `EgressProxy`,
  `EgressInvocation`, `CallGateway`, `denial_for_confined_command`,
  `suggest_host_pattern`, `host_allow_rule`, `extract_denied_host`,
  `EGRESS_CONNECT_TIMEOUT`, `EGRESS_IDLE_TIMEOUT`, `EGRESS_EVENT_CAPACITY`,
  `SANDBOX_DENIED_REASON`, `PRIVATE_ADDRESS_DENIED_REASON`,
  `HOST_DENIED_EXPLANATION`. `EgressShared`/`CallGateway` expose
  `take_private_address_denial` alongside `take_denied_host`/`denied_hosts`.
- Host identity (`credentials.rs`, crate-internal): `host_identity_env`,
  `isolated_config_env`, `needs_git_writes`.
- Git network policy (`git_network.rs`): `set_egress_connect_exe`.

## Invariants

- `ToolRegistry::call` validates against the compiled JSON schema first and
  never invokes a handler on validation failure; failed validations consume the
  `ValidationBudget` retry allowance.
- Tool names are canonicalized before lookup: `rg` → `grep`,
  `search_replace`/`edit_file` → `edit`.
- `ToolContext::resolve_path`/`resolve_write_path` reject empty paths, any
  `..` component, symlinks or ancestors that escape the canonical workspace
  root or session temp directory, and refuse writes anywhere below `.git` or
  `.forge`. The `.forge` check is judged below the workspace root, so a managed
  session whose worktree lives under `.forge/local/worktrees` stays writable.
- Validation happens before execution; `prepare_call` exists only for
  immutable read-only calls validated immediately before scheduling.
- Descriptors are sorted by name and cached until the next `register`.
- Confinement is decided at spawn and is fail-closed: if the host can sandbox
  but the policy cannot be built, the call is refused rather than run
  unconfined. `unconfined_shell` is only set for an approved exact retry.
  The other unconfined path is `run_shell_command_unconfined` (no sandbox, no
  egress gateway), which the approve-all session mode uses for background
  shells.
- The OS sandbox mounts the workspace's `.git` and `.forge` read-only
  (`readonly_subpaths`). `with_git_writable` opens `.git` for git mutation
  commands but always keeps `hooks` and `config` read-only (in `.git` and in
  linked-worktree gitdirs, on both Seatbelt and bubblewrap), because those
  name programs a later unconfined git would execute.
- Model-triggered child processes never inherit provider credentials: the
  shell family (bash, background jobs, `exec_command`), the `git` tool, and the
  network-git preflight's local config reads (`LocalRepo::git`) all strip
  `PROVIDER_CREDENTIAL_ENV`, the session's
  `credential_env_names` (custom `api_key_env` names), and any
  `FORGE_*_API_KEY`. Host-side `git credential fill` (the user's own
  credential helper, cached per host) strips the fixed names and
  `FORGE_*_API_KEY` but not the session's custom names, which do not reach
  that path. Confined shells also strip host credentials
  (`HOST_CREDENTIAL_ENV`: `GH_TOKEN`, `GITHUB_TOKEN`, `CARGO_REGISTRY_TOKEN`,
  ...); login shells are not used. When the egress grant allows a host,
  `host_identity_env` projects that host's HTTPS identity back in: it runs
  host-side `git credential fill` (cached per host for the process in
  `CREDENTIAL_CACHE`) and exposes the result through a spawn-local credential
  helper/gitconfig and a `{first-label}_TOKEN` variable.
- `background_run`, `ask_user_question`, `request_unconfined_retry`, and agent
  orchestration tools are intercepted by the agent loop before `call`; their
  `Tool::call` paths are defensive fallbacks.
- `update_plan` is stateless server-side: each call replaces the displayed plan
  and emits a `plan_update` turn event.
- `apply_patch` is capped at 8 MiB of patch text per call and writes only
  through the same confined path resolution.
- `web_fetch` follows redirects manually and re-checks every hop against the
  session egress policy; non-http(s) schemes are refused.
- `git` network subcommands are pre-checked against the host allowlist and then
  routed through the call gateway so unseen hosts are still refused.
- `EgressPolicy` matching: exact host, `*.domain` (subdomains only),
  `**.domain` (apex and subdomains), and bare `*` (every host); a deny always
  beats an allow. Hosts with an empty label (leading `.` or `..`) are never
  permitted.
- A host that resolves to a non-public (loopback, private, metadata) address
  is refused with `PRIVATE_ADDRESS_DENIED_REASON` unless an allow pattern names
  that host exactly (or `EgressOptions::allow_private_destinations` is set); wildcard grants
  never reach private destinations, and no grant prompt is offered for it.
- The egress event log is bounded at `EGRESS_EVENT_CAPACITY`; overflow drops the
  oldest event and increments `dropped_events`.
- `EgressProxy`/`EgressInvocation`/`CallGateway` stop listening when dropped;
  the Unix socket file is removed on drop.

## Side Effects

- Filesystem reads and writes in the workspace and the session temp directory;
  patches, edits, and writes modify repository files.
- Spawns shells, git, background jobs, PTY/pipe sessions, and subagent work.
- Starts loopback TCP listeners and (on Linux/macOS sandbox paths) Unix socket
  files for the egress proxy; logs connection events.
- Reads process environment variables (provider keys, API keys, proxy vars) and
  rewrites the child environment.
- Creates per-session temporary directories outside the repository. Every
  confined spawn also gets a host-identity dir (`xdg-config`, `xdg-cache`,
  `xdg-state`, and a `gitconfig` file): under the session temp dir when there
  is one, otherwise at `<workspace>/.forge-host-identity/` inside the
  workspace.
- Runs host-side `git credential fill` for granted hosts during confined
  spawns.
- Performs outbound network requests for `web_fetch` and network-capable
  `git` commands. `web_search` has only a mock backend and is never
  registered in a session, so it does no network I/O.
- Loads files and images from disk for `read_file`, `ls`, `view_image`, and
  `load_skill`.

## Verification

- Run: `cargo test --package forge-tools --locked`

Subdirectory contracts:

- [web_search](./web_search/EGM.md) — search tool, backend trait, and mock.
