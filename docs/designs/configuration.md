# Configuration design

**Status:** Shipped (product) — **config file optional**  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**Phase:** **1 only** (exclusive) — Phase 1 config keys only  
**PRD:** Portability NFRs, multi-provider  
**Architecture:** §9, decisions #12, #15  
**Related:** [model-providers.md](./model-providers.md), [surfaces.md](./surfaces.md)

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

[[mcp.servers]]
id = "example"
transport = "stdio"
command = "…"
args = []

[tui]
# theme / show_sidebar etc. later
```

**Phase 2+ keys** (not owned by this doc): context/offload/progress, sandbox, hitl, acl, otel — defined in Phase 2/3 designs ([context-lifecycle.md](./context-lifecycle.md), [governance.md](./governance.md), [observability.md](./observability.md)).

**Phase 5 keys** (not owned by this doc): `provider = "litellm"`, `[model.litellm]` — [litellm-config.md](./litellm-config.md).

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

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document (Phase 1 keys) | **1** |
| Exit | `forge.toml` + env run a Phase 1 session |

## 7. Open questions

1. Strict vs permissive unknown keys.  
2. ACL pattern language (glob vs regex vs structured).  
3. Encrypted secrets file support (or vault-only).

## Related docs

- [model-providers.md](./model-providers.md)  
- [tui-commands.md](./tui-commands.md) (`/model` mutates session config)  
