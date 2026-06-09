# Tool protocol design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**PRD:** CORE-01  
**Architecture:** §4.2 Tools, §10 Security  
**Related:** [agent-loop.md](./agent-loop.md), [governance.md](./governance.md), [protocols-mcp-acp.md](./protocols-mcp-acp.md)

---

## 1. Problem / context

Models emit structured tool calls as JSON. Without a hard contract, invalid arguments reach handlers and produce partial side effects or opaque failures. Enterprise and AI-coding UX both need **fail-closed validation** and **self-correcting retries**.

## 2. Goals & non-goals

**Goals**

- Every tool (built-in or MCP) has a declared input schema and serializable output.  
- Invalid args never reach side effects; the model receives a structured error and may retry.  
- Tool listings shown to the model match enforceable contracts (post-ACL).  
- High AI-codability: adding a built-in is “types + handler,” not framework boilerplate.

**Non-goals**

- Graph/DAG tool orchestration DSL.  
- Trusting MCP server descriptions without runtime validation of call args.  
- Storing secrets inside tool schemas or descriptions.

## 3. Design

### 3.1 Tool contract

| Field | Description |
|-------|-------------|
| `name` | Stable identifier (`grep`, `read_file`, `mcp__server__tool`) |
| `description` | Model-facing text (no secrets) |
| `input_schema` | JSON Schema (from `schemars` for built-ins; from MCP for remote) |
| `handler` | Built-in async fn, or MCP call bridge |
| `side_effect_class` | `read` \| `write` \| `network` \| `exec` \| `meta` (for policy) |
| `idempotent` | bool — replay may retry incomplete intents only if true |
| `requires_hitl` | optional static hint; governance may still override |

### 3.2 Built-in vs MCP

| Source | Schema origin | Dispatch |
|--------|---------------|----------|
| Built-in | Rust types + `Serialize`/`Deserialize` + `JsonSchema` | In-process handler |
| MCP | `tools/list` JSON Schema | MCP `tools/call` after ACL |

Names from MCP are namespaced to avoid collisions (e.g. `mcp:<server_id>:<tool>` or equivalent stable form).

### 3.3 Registry lifecycle

1. Register built-ins at process start.  
2. Discover MCP servers from config; fetch tool lists.  
3. Merge catalogs.  
4. Apply ACL filter (**governance**) → **visible set** for the model.  
5. On call: resolve by name → schema validate → authorize → journal intent → execute → journal result.

### 3.4 Validation

- Deserialize JSON args into the tool’s input type / schema.  
- On failure: produce `ToolValidationError { tool, path, message, schema_hint }`.  
- Do **not** journal a successful `tool_result`; journal a validation failure event (or failed intent) so recovery is honest.  
- Inject a **system/tool error turn** (or harness message) prompting the model to correct args.  
- Retry budget: default **max 3** validation failures per tool name per turn; then fail the turn with a clear error.

### 3.5 Output

- Built-ins return typed values serialized to JSON for the model/tool message.  
- Oversized outputs are handed to **context lifecycle** for offload (CTX-01); tool protocol only returns the body or a stub + URI placeholder as directed by context.

### 3.6 Coding built-ins (Phase 1 default set)

| Tool | Class | Notes |
|------|-------|-------|
| `read_file` | read | path, optional offset/limit |
| `write_file` / `edit_file` | write | worktree-aware paths |
| `bash` | exec | sandbox + HITL policies apply |
| `grep` / `search` | read | |
| `git_*` (status, diff, commit, …) | write/network | push-class → often HITL |

Exact names can refine in implementation; classes drive policy.

## 4. Interfaces (sketch)

```rust
// illustrative
trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> schemars::schema::RootSchema;
    fn side_effect_class(&self) -> SideEffectClass;
    fn idempotent(&self) -> bool;
    async fn call(&self, ctx: &ToolContext, args: serde_json::Value) -> Result<ToolOutput, ToolError>;
}
```

`ToolContext` carries workspace root, worktree path, principal, cancellation, and redacting logger—not raw vault secrets (governance injects at the edge).

## 5. Failure modes & edge cases

| Case | Behavior |
|------|----------|
| Unknown tool name | Error to model; no side effects |
| ACL deny after list race | Deny at call; audit |
| Schema valid but policy deny | Deny; no execute |
| Handler panic / timeout | Failed `tool_result`; no silent success |
| MCP server down | Discovery/call error; tools absent or call fails closed |

## 6. Phase / rollout

| Phase | Scope |
|-------|-------|
| 1 | Built-ins + schema validation + registry; MCP tools before Phase 1 ends |
| 2 | Tighter coupling to vault inject metadata; richer side_effect_class policies |
| 3 | Channel principals with restricted default tool sets |

## 7. Open questions

1. Exact MCP name mangling scheme (prefix vs server map).  
2. Whether validation retries count toward `max_turns`.  
3. Unified vs per-tool timeout defaults.

## Related docs

- [agent-loop.md](./agent-loop.md)  
- [governance.md](./governance.md)  
- [context-lifecycle.md](./context-lifecycle.md)  
- [../architecture.md](../architecture.md)  
