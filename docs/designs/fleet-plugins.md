# Fleet plugins design (SCIM + SIEM)

**Status:** Shipped (library only — not CLI)  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**Phase:** **3 only** (exclusive)  
**PRD:** FLEET-01  
**Architecture:** Phase 3  
**Related:** [observability.md](./observability.md), [governance.md](./governance.md)

---

## 1. Problem / context

Enterprise fleets need identity provisioning (SCIM) and audit export to SIEM without forking the harness core.

## 2. Goals & non-goals

**Goals**

- Plugin interfaces for SCIM provisioning hooks.  
- SIEM-oriented export of audit records (e.g. OTLP/compatible).  
- Load without core code changes.

**Non-goals**

- Built-in proprietary APM UI.  
- Replacing governance policy engine (Phase 2).

## 3. Design

| Plugin | Responsibility |
|--------|----------------|
| SCIM | Provision/deprovision principals and role bindings used by ACL |
| SIEM export | Stream redacted audit events to customer SIEM |

## 4. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **3** |
| Exit | Plugins load via config; sample SCIM + SIEM path works in staging |

## 5. Open questions

1. SCIM subset (Users/Groups only vs full).  
2. Default SIEM encodings (OTLP logs vs CEF).

## Related docs

- [observability.md](./observability.md)  
- [channels.md](./channels.md)  
