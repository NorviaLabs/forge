# forge-connect

## Overview

Provider connection crate (CONN-01): built-in and user-defined provider specs,
OAuth and API-key auth, the credential store, non-secret preferences, remote
model catalogs, picker routes, and credential verification. It performs
synchronous HTTP with `ureq` only (never `reqwest::blocking`, which panics
inside a Tokio runtime). Source lives in `src/`.

## Subdirectories & Boundaries

- [src](./src/EGM.md) — profiles, credentials, catalog, OAuth, and connect
  service.

## Interface

- No symbols at this level. The crate surface (`ConnectRegistry`,
  `ProviderSpec`, `CredentialStore`, `PreferenceStore`, `ConnectService`,
  `ModelCatalogCache`, `Credentials`, `RouteTable`, picker routes, OAuth
  clients, and the embedded built-in definitions in `providers/*.toml`) is
  re-exported from `src/lib.rs` and documented in [./src/EGM.md](./src/EGM.md).

## Invariants

- Secrets live only in the 0600 credentials file; preferences, provider TOML,
  and catalog caches never hold keys or tokens.
- Connecting a provider stores credentials only; it never selects a model.
- Removing a custom provider (`ConnectService::remove_custom_provider`) edits
  `providers.toml` first, in one checked write, then clears its key, cached
  models, and saved selections; adding an id clears the same state first.
- The profile id is the only provider identity and the model-id prefix.
  Built-in behavior lives in `providers/*.toml`; production code never names
  a built-in id (`tests/no_profile_id_branches.rs`).
- Synchronous HTTP uses `ureq` only, so callers may invoke this crate from
  inside a Tokio runtime.

## Side Effects

- Reads/writes `credentials.toml`, `preferences.toml`, the model catalog cache,
  and the user `providers.toml` under the platform config dir.
- Outbound HTTP for catalogs, models.dev, credential verification, OAuth, and
  usage reporting; may open a browser for OAuth.

## Verification

- Run: `cargo test --package forge-connect --locked`
