# Governance & sandbox design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**Phase:** **2 only** (exclusive)  
**PRD:** SEC-01, SEC-02, SEC-03  
**Architecture:** §2.2, §10, §14 Phase 2  
**Related:** [tool-protocol.md](./tool-protocol.md), [durable-hitl.md](./durable-hitl.md)

---

## 1. Problem / context

Enterprise agent harnesses fail when secrets leak into prompts, tools are over-permissioned, or execution is unsandboxed. Governance is on the **tool path** and credential path, not an afterthought.

## 2. Goals & non-goals

**Goals**

- Vault (or env) inject credentials at call time; never in model context (SEC-01).  
- Dynamic ACLs hide unauthorized tools from listings and deny calls (SEC-02).  
- Progressive sandbox depth: light → container → eBPF profiles (SEC-03).  
- Immutable audit of decisions and invocations (redacted).

**Non-goals**

- Full IdP/SCIM in Phase 2 core (Phase 3 plugins).  
- Guaranteeing host security if operators disable sandbox.

## 3. Design

### 3.1 Principal

```text
Principal { id, roles[], scopes[], surface, session_id }
```

Local TUI default: `local-dev` with broad allow (configurable). Channel surfaces: restricted principal by default.

### 3.2 Tool ACL (SEC-02)

**On `tools/list` (model-facing):**

1. Merge built-in + MCP catalog.  
2. Filter by principal allow/deny rules.  
3. Return only visible tools.

**On `tools/call`:**

1. Re-check ACL (no TOCTOU rely on list alone).  
2. Deny → journal + audit; model sees denial error.

Rule language (initial): tool name globs + optional side_effect_class matchers. Exact syntax TBD (configuration open question).

### 3.3 Credential broker (SEC-01)

| Step | Behavior |
|------|----------|
| Resting state | Secrets in vault/env only |
| Model call | HTTP client auth injected in provider adapter |
| Tool call | Governance maps tool → required secret keys → inject into env/headers for handler only |
| After call | Do not persist secrets into journal plaintext or tool messages |
| Traces | Redact known secret patterns and auth headers |

### 3.4 Policy classify (allow / deny / hitl)

```text
validate → ACL → classify(side_effect, tool, args, principal)
  allow  → inject → sandbox exec
  deny   → audit → error
  hitl   → journal hitl_wait → pause
```

High-risk examples: `git push`, unrestricted network, production deploy tools.

### 3.5 Sandbox profiles (SEC-03)

| Profile | Phase | Controls |
|---------|-------|----------|
| `light` | 1 default | cwd/worktree limits, process per tool, basic env scrubbing |
| `container` | 2 | non-root, restricted egress, read-only root FS |
| `ebpf` | 2+ | kernel hooks; kill on policy breach; &lt;1 ms target when enabled |

### 3.6 Audit record (fields)

- timestamp, session_id, principal  
- tool name, redacted args  
- decision (allow/deny/hitl), policy id  
- result status, duration  
- trace_id  

Export via OTLP later ([observability.md](./observability.md)).

## 4. Interfaces

```rust
trait Authorizer {
    fn filter_tools(&self, principal: &Principal, tools: Vec<ToolInfo>) -> Vec<ToolInfo>;
    fn authorize(&self, principal: &Principal, call: &ToolCall) -> Decision;
}

trait SecretBroker {
    async fn materialize(&self, keys: &[SecretRef]) -> Result<SecretMaterial, VaultError>;
}

trait Sandbox {
    async fn exec(&self, req: ExecRequest) -> Result<ExecResult, SandboxError>;
}
```

## 5. Failure modes & edge cases

| Case | Behavior |
|------|----------|
| Vault unavailable | Fail closed for tools needing secrets; model tools without secrets may proceed |
| ACL misconfig allows all on channel | Mitigate with secure defaults for channel principals |
| Sandbox escape | Defense in depth; audit; do not claim perfect isolation on `light` |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **2** |
| Exit | SEC-01/02/03 metrics met |
| SCIM principals / SIEM stream | [fleet-plugins.md](./fleet-plugins.md) (Phase 3) |

## 7. Open questions

1. Vault product integrations (HashiCorp, cloud SM, 1Password) priority.  
2. Argument-level ACL (e.g. deny path `**/.env`).  
3. Default HITL list for coding built-ins.

## Related docs

- [tool-protocol.md](./tool-protocol.md)  
- [durable-execution.md](./durable-execution.md) HITL  
- [surfaces.md](./surfaces.md) channel ACL  
