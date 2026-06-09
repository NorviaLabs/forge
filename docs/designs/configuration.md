# Configuration design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**PRD:** Portability NFRs, multi-provider  
**Architecture:** §9, decisions #12, #15  
**Related:** [model-providers.md](./model-providers.md), [governance.md](./governance.md), [surfaces.md](./surfaces.md)

---

## 1. Problem / context

Operators need predictable configuration for model, journal, MCP, ACLs, and surfaces—without code changes for provider switches. CI needs env overrides; local dev needs a simple file.

## 2. Goals & non-goals

**Goals**

- TOML file + env overrides.  
- Workspace root **defaults to process cwd**.  
- Single place for model provider switch.  
- Secrets prefer env/vault over committed files.

**Non-goals**

- Remote dynamic config service in Phase 1.  
- GUI settings app.

## 3. Design

### 3.1 File locations (merge order)

Highest precedence wins:

1. CLI flags  
2. Environment variables  
3. Project file: `./forge.toml` (if present)  
4. User file: `~/.config/forge/config.toml` (XDG)  
5. Built-in defaults  

### 3.2 Schema (illustrative)

```toml
workspace_root = "."   # optional; default cwd

[model]
provider = "anthropic"
model = "claude-sonnet"
# base_url = "http://127.0.0.1:8000"

[journal]
backend = "sqlite"
path = ".forge/sessions"

[context]
offload_token_threshold = 2000
reset_usage_ratio = 0.80
offload_dir = ".forge/offload"
progress_path = ".forge/progress.json"

[sandbox]
profile = "light"      # light | container | ebpf (later)

[hitl]
# patterns or classes requiring approval
require_for = ["network:git_push", "exec:unrestricted"]

[[mcp.servers]]
id = "example"
transport = "stdio"
command = "…"
args = []

[acl]
# principal defaults for local tui
default_principal = "local-dev"

[[acl.rules]]
principal = "local-dev"
allow = ["*"]
deny = ["bash:rm -rf *"]  # illustrative pattern language TBD

[otel]
enabled = false
endpoint = ""

[tui]
# theme / show_sidebar etc. later
```

### 3.3 Env overrides (examples)

| Env | Maps to |
|-----|---------|
| `FORGE_MODEL_PROVIDER` | `model.provider` |
| `FORGE_MODEL_ID` | `model.model` |
| `FORGE_API_KEY` | provider credential (dev) |
| `FORGE_WORKSPACE` | `workspace_root` |
| `FORGE_JOURNAL_PATH` | `journal.path` |
| `FORGE_OTEL_ENDPOINT` | `otel.endpoint` |

### 3.4 Workspace root

- If unset: `std::env::current_dir()` at start.  
- All relative paths (journal, offload, worktrees) resolve under workspace unless absolute.  
- `AGENTS.md` discovery: workspace root first.

### 3.5 Validation

- Unknown keys: warn (forward compatible) or strict mode flag.  
- Invalid provider: fail at startup with clear error.  
- ACL deny that blocks all tools: warn.

## 4. Interfaces

- `Config::load() -> Result<Config, ConfigError>`  
- `Config::workspace_root() -> PathBuf`  
- CLI: `forge --config path --workspace path …`

## 5. Failure modes & edge cases

| Case | Behavior |
|------|----------|
| Missing config file | Defaults + env |
| Conflicting project vs user keys | Project wins over user; CLI/env win over both |
| Relative journal path + changed cwd mid-run | Freeze root at session start |

## 6. Phase / rollout

| Phase | Scope |
|-------|-------|
| 1 | model, journal, mcp list, workspace, basic tui/headless |
| 2 | acl, hitl, sandbox, context thresholds |
| 3 | otel export, channel configs |

## 7. Open questions

1. Strict vs permissive unknown keys.  
2. ACL pattern language (glob vs regex vs structured).  
3. Encrypted secrets file support (or vault-only).

## Related docs

- [model-providers.md](./model-providers.md)  
- [tui-commands.md](./tui-commands.md) (`/model` mutates session config)  
