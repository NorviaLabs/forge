# forge-test-support

## Overview

Test-only fixtures shared across Forge crates: a scripted-response HTTP server
over a real TCP socket, and helpers that create deterministic Git working
copies. The crate has no runtime dependency; it is pulled in as a
dev-dependency by tests in other crates.

## Subdirectories & System Boundaries

- `src`: Component package containing all fixtures. Contract:
  [`./src/EGM.md`](./src/EGM.md).

## Interface

- Child component contract: [`./src/EGM.md`](./src/EGM.md) — the public
  symbols, invariants, and side effects implemented by this crate.
- Crate-level façade exports and verification target are documented there.

## Invariants

- The HTTP mock is a real socket server, so it works for both blocking and
  async clients; responses are consumed in the given order, one per accepted
  connection. A connection whose request cannot be read consumes its scripted
  response without sending it, so later connections get later responses.
- Helper names and behavior are stable across crates; changing a fixture must
  keep every dependent test's assumptions valid.
- Git fixtures use a deterministic initial branch (`main`) and committer
  identity, because a test host may have no global Git config.
- Nothing here is compiled into production binaries; it exists only for
  `#[cfg(test)]` consumers via dev-dependencies.

## Side Effects

- Spawns a background listener thread bound to `127.0.0.1:0`.
- Runs `git init/config/add/commit` and writes `f.txt` in the fixture
  directory.
- Panics (rather than returning errors) on fixture setup failures or when
  `git` is missing from `PATH`.

## Verification

- Run: `cargo test --package forge-test-support --locked`
