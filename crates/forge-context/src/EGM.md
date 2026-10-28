# forge-context src — Context Lifecycle

## Overview

Implements CTX-01/CTX-02: large tool-output offload, progress checkpointing,
skill discovery, and the compaction subsystem. `ContextEngine` is a cheap,
cloneable handle scoped to one session and workspace; offloaded bodies and
progress documents are written under the centralized runtime-storage resolver
(`.forge/local/cache`) or the configured paths.

## Interface

- `ContextEngine` — public fields `config`, `workspace`, `session_id`,
  `goal`; `new(workspace, session_id)`, `usage_ratio`,
  `offload_tool_output`, `maybe_offload_tool_content`, `image_cache_dir`,
  `progress_path`, `write_progress`, `read_progress`, `load_agents_md`,
  `load_skills`, `skill_count`, `refresh_skills`.
- `ContextConfig` — `offload_token_threshold` (2000), `capacity_tokens`
  (500 000), `offload_dir` (`.forge/offload`), `progress_path`
  (`.forge/progress.json`).
- `OffloadResult { uri, sha256, bytes, tokens_estimate, summary, in_context }`.
- `ContextError` — `Io`, `Json`, `Other`.
- `estimate_tokens`, `estimate_messages_tokens`, `reduction_ratio`.
- `SkillManifest { name, description, dir, body, has_frontmatter, metadata,
  compatibility, license }`.
- `discover_skills(workspace) -> Vec<SkillManifest>`.

Subdirectory:

- [compaction](./compaction/EGM.md) — checkpoint schema, policy arithmetic,
  protected facts, tail selection, and candidate planning.

## Invariants

- `estimate_tokens` is a ~4-characters-per-token heuristic over Unicode scalars
  and always returns at least 1.
- Offload happens only when the body exceeds the configured token threshold; the
  result always includes the URI, SHA-256, byte size, and a 500-character
  summary. The in-context replacement targets at least an 80% token reduction;
  this is covered by tests, not enforced at runtime (the `debug_assert!` in
  `offload_tool_output` holds for any non-empty body, so it does not check the
  ratio).
- When `offload_dir`/`progress_path` retain their defaults, resolution routes
  through `LocalRuntimeStorage`; explicit overrides are respected verbatim.
- `progress_path()` performs no I/O; `write_progress` creates parent
  directories and establishes storage exclusion/fallback first. By contrast,
  `image_cache_dir()` and offload resolve the default offload base through
  `LocalRuntimeStorage::path_for`, which creates the cache directory and
  initializes runtime storage.
- Skill discovery reads `.agents/skills/*/SKILL.md` in the workspace and the
  global skills directory (`FORGE_GLOBAL_SKILLS_DIR` overrides
  `~/.agents/skills`), sorts by name, and lets project skills win on collisions.
- A skill with parseable `name` + `description` frontmatter contributes only
  those to the prompt; otherwise the whole body is kept and `has_frontmatter`
  is false.
- The skills list is cached in `ContextEngine` until `refresh_skills`.
- `load_agents_md` returns an empty string when the file is missing.

## Side Effects

- Creates directories and writes offloaded tool bodies to disk; resolving the
  default offload base (including from `image_cache_dir()`) creates the
  runtime-storage cache directory and triggers its lazy setup.
- Writes and reads the progress document.
- Reads `AGENTS.md` and every discovered `SKILL.md` from the workspace and the
  global skills directory.
- Reads environment variables for the global skills directory override.

## Verification

- Run: `cargo test --package forge-context --locked`

Subdirectory contracts:

- [compaction](./compaction/EGM.md) — compaction engine.
