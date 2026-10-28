# forge-model

## Overview

Crate root for the model layer. There is no source code at this level: the
implementation lives in `src/`, and integration tests live in `tests/` (no
contract there).

## Subdirectories & Boundaries

- `src/`: [./src/EGM.md](./src/EGM.md) — `ModelClient` trait, `ModelError`, `SharedMessages`, wire normalization, prompt-wire/prompt-cache diagnostics, image attachments, mock client, and native transport dispatch.

## Interface

- No symbols at this level. The crate surface is the `pub use` re-exports and items in `src/lib.rs` (`ModelClient`, `ModelRequest`, `ModelError`, `SharedMessages`, `StreamEventTx`, `client_from_config`, `MockModelClient`, `NativeModelClient`, normalization and prompt-wire helpers, image freezing); see [./src/EGM.md](./src/EGM.md).

## Notes

- `tests/` holds integration tests only and is intentionally undocumented.
- This crate must stay free of session, tool, and UI concerns; it depends on `forge-types`, `forge-config`, and `forge-connect` for route and provider metadata.

## Invariants

- Every model call goes through the `ModelClient` trait; callers never construct provider-specific requests.
- `forge-model` may depend on `forge-config` and `forge-connect` but never on `forge-core`, `forge-session`, `forge-tools`, or `forge-tui`.

## Side Effects

- Outbound provider HTTP traffic originates only inside the native transports.
- Workspace image files are read by `image::load_image_ref`: from the native transports (through their image cache), and also outside `native/` by the default `ModelClient::prompt_wire` (used by `MockModelClient`, via `prompt_wire::openai_compat_prompt` → `normalize::forge_messages_to_wire_in`), the public `forge_messages_to_wire`, and `apply_missing_image_notes`.
- `freeze_attachments` copies attachment bytes into a caller-supplied cache directory at message insert.
- Dispatch is by the model id's provider prefix (`RouteTable::profile_for_request`) and that profile's `wire`, through a `forge_connect::RouteTable` resolved from the loaded registry and the shared `forge_connect::Credentials` handle; no code branches on a built-in provider id (guarded by `crates/forge-connect/tests/no_profile_id_branches.rs`).

## Verification

- Run: `cargo test --package forge-model --locked`
