# forge-tools — Tool Protocol Implementation

## Overview

Implements the schema-validated tool protocol (`Tool` trait + `ToolRegistry`)
and every built-in tool. This crate is the trust boundary between model-authored
JSON and the machine: it validates arguments before dispatch, confines file
paths to the workspace (plus an optional session scratch directory), confines
shell processes through OS sandboxes, strips provider credentials from child
processes, and routes network access through a domain-filtered egress proxy.

Subdirectories:


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
  (`ask_user_question`), `default_builtins`, `run_shell_command`,
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
  `SendMessageTool` (queue, `wake`, or `interrupt`), `WaitAgentTool` (also
  lists descendants with `since_revision: 0`), with `SpawnAgentArgs`,
  `AgentMode`, `AgentMessageArgs`, `AgentMessageAction`, `WaitAgentArgs`.
- Skills: `LoadSkillTool` (`load_skill`), `LoadSkillArgs`.
- Media/fetch: `ViewImageTool` (`view_image`), `ViewImageArgs`;
  `WebFetchTool` (`web_fetch`), `WebFetchArgs`, `web_fetch_tool`.
- Validation: `validate_args`, `validation_error_signature`,
  `ValidationBudget` (`new`,
  `with_default_max`, `record_failure_with_signature`).
- Output shaping: `compress_tool_output`.
- Sandbox (`sandbox.rs`): `SandboxPolicy` (`for_workspace`,
  `with_read_only_workspace`, `workspace_writable`, `with_command_access`,
  `toolchain_env`, `with_read_path`, `with_git_writable`, `with_session_tmp`,
  `with_home`, `with_tool_cache`, `tool_cache`, `default_tool_cache`,
  `with_egress_proxy`, `egress_proxy_port`, `with_egress_socket`,
  `egress_socket`, `workspace_root`, `with_egress`),
  `Unavailable`
  (`reason`), `availability`,
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
  `suggest_host_pattern`, `host_allow_rule`,
  `EGRESS_CONNECT_TIMEOUT`, `EGRESS_IDLE_TIMEOUT`, `EGRESS_EVENT_CAPACITY`,
  `SANDBOX_DENIED_REASON`, `PRIVATE_ADDRESS_DENIED_REASON`,
  `HOST_DENIED_EXPLANATION`. `EgressShared`/`CallGateway` expose
  `take_private_address_denial` alongside `take_denied_host`/`denied_hosts`.
- Host identity (`credentials.rs`, crate-internal): `host_identity_env`
  (takes the workspace root), `isolated_config_env`, `needs_git_writes`.
- Git network policy (`git_network.rs`): `set_egress_connect_exe`.

- `egress_connect` (`pub mod`): `SUBCOMMAND = "__egress-connect"` and `run(args) -> i32` (0 tunnel closed cleanly, 1 refused or I/O failure, 2 usage or bad port). Carries one TCP stream through the egress gateway with `CONNECT`; rejects hosts with whitespace or control bytes; brackets IPv6. `git_network` writes `<current executable> __egress-connect …` into SSH's ProxyCommand and the `git://` proxy script, so **every executable that runs agents must intercept it before its own argument parsing**: `forge` (`forge-cli/src/main.rs`) and `forge-desktop` (`forge-desktop/src/main.rs`).

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
- Seatbelt read policy: the whole disk is readable except a deny-list
  (`HOME_SECRETS`, `HOME_PERSONAL`, `~/Library` apart from
  `LIBRARY_READABLE`, the per-user temp tree, `/Volumes`, other projects'
  `.forge/`). The workspace, session temp directory, tool cache, explicit
  read paths and linked git dirs are reopened after the denies; deny-list
  entries inside a reopened tree are denied again. Nothing derived from the
  command text changes what is readable. Matching is on resolved paths, so
  symlinks and case variants do not bypass it.
- Writes outside the workspace and session temp directory go only to the
  tool cache (`~/Library/Caches/forge/sandbox`, shared across sessions and
  projects, used by confined processes only). `toolchain_env` points
  `CARGO_HOME`, the npm/pnpm/yarn/bun/pip/uv/Go caches and `XDG_CACHE_HOME`
  there, links the user's `~/.cargo/config.toml` in, and gives npm a copy of
  `~/.npmrc` with credential lines removed. The user's own caches are
  read-only.
- `set_process_group` calls `setsid`: a spawned child is its own process
  group leader and has no controlling terminal.
- The OS sandbox mounts the workspace's `.git` and `.forge` read-only
  (private `readonly_subpaths_of`). `with_git_writable` opens `.git` for git mutation
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
  host-side `git credential fill` inside the session workspace (so
  repository-local `credential.helper` and `includeIf` rules apply), caches
  the result per `(workspace, host)` in `CREDENTIAL_CACHE`, and exposes it
  through a spawn-local credential helper/gitconfig and a
  `{first-label}_TOKEN` variable. One workspace's identity never answers for
  another's.
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
- Forge's own network git (`own_git.rs`): `register_workspace_egress`
  ties a workspace to a session's `EgressGrant`; `install_git_network`
  installs the `forge_sandbox::git::NetworkProvider` that, for a repository
  inside a registered workspace, starts a per-invocation gateway endpoint and
  SSH broker and supplies the credential-helper environment. Only installed
  on a multi-threaded runtime.
- `git credential fill` runs outside any repository, so only the user's
  global and system helpers apply; a repository's `credential.helper` is
  neither run nor believed. `credential_helper_env` replaces the helper list
  for a confined git through `GIT_CONFIG_*` without replacing the user's
  global config.
- SSH for confined git (`ssh_broker.rs`): every confined shell with a gateway
  gets `GIT_SSH_COMMAND=<forge exe> __ssh-broker <addr> <token>`. The client
  forwards git's ssh arguments and stdio to an `SshBroker` in the Forge
  process, which runs the real ssh on the host (through the invocation's
  gateway, so the host policy and hold-and-ask apply) and relays output and
  exit status. Keys and the agent socket never enter the sandbox. The broker
  accepts only what git sends (`parse_request`: `-p`, `-4`/`-6`,
  `-o SendEnv=GIT_PROTOCOL`, a destination, one git service with one quoted
  path) and composes the ssh command from the environment and global git
  config only (`git_network::host_ssh_command`), never the repository's.
  Both binaries dispatch `__ssh-broker` before argument parsing.
- Signing for confined git (`sign_broker.rs`): Forge's own git
  (`own_git.rs`) gets `gpg.program`, `gpg.<format>.program` set through
  `GIT_CONFIG_*` to wrapper scripts in its scratch directory that run
  `<forge exe> __sign-broker <addr> <token> <format> "$@"`. The client sends
  the payload to a `SignBroker` in the Forge process, which runs the user's
  signing program on the host. The broker signs only a commit, a tag or a
  push certificate (`is_git_payload`), takes format, program and key from
  the user's git config with the repository's scopes skipped
  (`git_network::host_signer`), and does not verify. Confined shells with
  a gateway get it too (`shell_spawn.rs`).
- Git in a confined shell reads the user's own git config (`~/.gitconfig`,
  and `$XDG_CONFIG_HOME/git` linked into the isolated XDG tree). Forge adds
  settings through `GIT_CONFIG_*` only: the credential helper list is
  replaced (`credentials::host_identity_env`), `http.proxy` and
  `core.gitProxy` point at the gateway, and the signing programs at the
  broker. There is no SSH-to-HTTPS rewrite any more.
- Credentials stay outside the sandbox (`tls_intercept.rs`,
  `credentials::behind_the_gateway`). A confined spawn's credential store and
  `{LABEL}_TOKEN` hold a placeholder (`EgressShared::supply_credential`);
  the session's in-memory authority's certificate is appended to the system
  roots in a bundle the spawn trusts (`GIT_SSL_CAINFO`, `SSL_CERT_FILE`,
  `CURL_CA_BUNDLE`, `REQUESTS_CA_BUNDLE`, `NODE_EXTRA_CA_CERTS`). An
  endpoint whose client was given that bundle terminates `CONNECT host:443`
  for a host with a registered credential (`serve_terminated`), replaces the
  placeholder in the forwarded request (`supply_credentials`), one request
  per connection, and speaks verified TLS to the host. An `Authorization`
  without the placeholder is refused with a 403. Other hosts and other
  endpoints keep opaque tunnels. A client that rejects the certificate is
  reported by `untrusting_note`.
- Writes under the user's identity are gated (`write_gate.rs`,
  `EgressShared::admit_write`). A request the gateway put the credential
  into is classified: `GET`/`HEAD`/`OPTIONS` and `git-upload-pack` are
  reads; `git-receive-pack`, other methods and GraphQL mutations (the body is
  read, up to 1 MiB) are writes with a target (`host/owner/repo`, or the
  host). The SSH broker's `Authorize` hook does the same for
  `git-receive-pack`. A write goes through when `EgressPolicy::permits_write`
  (rules `remote_write(target)`), else it is held and asked through the same
  `HostAsk` channel with `write: Some(summary)` and `host` set to the
  target. With no asker it is refused.
- `own_git::Provider` always opens a network: the registered session's
  grant, or for a workspace no session governs, a standalone gateway with
  the project's permissions plus the built-in hosts and no asker.
  The SSH broker also accepts `git-lfs-authenticate <path> download|upload`;
  an upload is gated like a push.
- Built-in allow-list (`EgressPolicy::with_builtin_hosts`, applied by
  `forge_core::egress_policy_for_workspace`): `BUILTIN_CODE_HOSTS`
  (github.com, gitlab.com, bitbucket.org, with subdomains) and
  `BUILTIN_REGISTRY_HOSTS`. A deny rule wins. Code hosts are `inspected`:
  an endpoint whose client trusts the authority terminates TLS to them even
  without a credential, and then refuses any `Authorization`
  (`Identity::Anonymous`). Registries are tunnelled. Every confined spawn
  with a live endpoint now gets the trust variables (`credentials::trust_env`,
  each keeping the user's own bundle if set) and
  `CARGO_NET_GIT_FETCH_WITH_CLI=true`.
- `gh` cannot trust the authority (Go on macOS uses the system verifier
  only), so `GH_CONFIG_DIR` points it at a config with `http_unix_socket`
  set to the invocation's Unix socket. That socket alone accepts origin-form
  requests (`ProxyRequest::Origin`): the gateway applies the host policy,
  does the TLS and supplies the credential. The Seatbelt profile allows
  exactly that socket path.
- The `git` tool runs confined through the shell spawn path
  (`builtins::run_git_confined`): argument validation and the host
  pre-check first, then `git <sub> <quoted args>` as a confined shell
  command, with per-URL proxy settings pointed at `$HTTP_PROXY`. An approved
  unconfined retry (`ctx.unconfined_shell`) or a host with no sandbox keeps
  the host path (`route_through_gateway`).
- Hold-and-ask: a host no rule decides (`EgressPolicy::undecided`) is not
  refused when an asker is set (`EgressShared::set_asker`). The gateway holds
  the connection (`admit`/`admission`, up to `EGRESS_HOLD_TIMEOUT`) and sends a
  `HostAsk`; `HostAnswer::Allow` (host already granted) or `AllowOnce` (this
  invocation only) lets the same connection through. A deny rule, a malformed
  name, no asker, or no answer refuses as before and queues the host for the
  approval that follows. A refusal by the user (`Admission::RefusedByUser`) is
  not queued: the shell tools append `user_refusal_note` to the output, skip
  denial classification and report `ExecutionOutcome::Denied`. `web_fetch` and
  the `git` tool pass an undecided host through their pre-check
  (`asks_about`) so the gateway asks.
- `EgressPolicy` matching: exact host, `*.domain` (subdomains only),
  `**.domain` (apex and subdomains), and bare `*` (every host); a deny always
  beats an allow. Hosts with an empty label (leading `.` or `..`) are never
  permitted.
- A host that resolves to a non-public (loopback, private, metadata) address
  is refused with `PRIVATE_ADDRESS_DENIED_REASON` unless an allow pattern names
  that host exactly (or `EgressOptions::allow_private_destinations` is set); wildcard grants
  never reach private destinations, and no grant prompt is offered for it.
- The egress event log is bounded at `EGRESS_EVENT_CAPACITY`; overflow drops the
  oldest event and increments an internal dropped counter.
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
  spawns, with the workspace as its working directory.
- Performs outbound network requests for `web_fetch` and network-capable
  `git` commands.
- Loads files and images from disk for `read_file`, `ls`, `view_image`, and
  `load_skill`.

## Verification

- Run: `cargo test --package forge-tools --locked`

