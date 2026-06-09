# Multi-channel ingress design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**Phase:** **3 only** (exclusive)  
**PRD:** CH-01  
**Architecture:** §5.10, §14 Phase 3  
**Related:** [governance.md](./governance.md) (Phase 2 ACL), [fleet-plugins.md](./fleet-plugins.md)

---

## 1. Problem / context

Operators want Slack/Telegram/webhook tasks on the **same durable core** without granting channel users broad repo execution rights.

## 2. Goals & non-goals

**Goals**

- Map channel messages → sessions.  
- **Restricted principal** by default (no broad repo tools).  
- Async ingress; not always-on unconstrained code executor.

**Non-goals**

- ACP (Phase 2) → [protocol-acp.md](./protocol-acp.md).  
- TUI/headless (Phase 1) → [surfaces.md](./surfaces.md).  
- SCIM/SIEM → [fleet-plugins.md](./fleet-plugins.md).

## 3. Design

```text
Slack/TG/webhook --> channel gateway --> session (restricted ACL) --> core loop
```

| Rule | Behavior |
|------|----------|
| Default ACL | Deny file write / unrestricted bash unless explicitly granted |
| Secrets | Never in channel transcripts |
| Durability | Same journal as other surfaces |

## 4. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **3** |
| Exit | Channel task completes under restricted tools; broad repo tools unavailable by default |

## 5. Open questions

1. Which channels in first ship set.  
2. Threading model (one session per thread vs per user).

## Related docs

- [governance.md](./governance.md)  
- [fleet-plugins.md](./fleet-plugins.md)  
