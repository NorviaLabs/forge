# Contributing to Forge

Thanks for helping improve Forge. Contributions are welcome through bug reports, documentation, tests, design discussion, and focused pull requests.

Forge is alpha software. Interfaces and behavior may change quickly, but changes should remain scoped, tested, and safe for users running an agent against real repositories.

## Before you start

- Search existing issues and pull requests before opening a duplicate.
- Use an issue template for bugs, setup problems, and feature requests.
- Discuss large features, architecture changes, new providers, or breaking behavior in an issue before implementation.
- Report vulnerabilities privately through [GitHub Security Advisories](https://github.com/NorviaLabs/forge/security/advisories/new). Do not open a public security issue.

## Development setup

Install Git and Rust 1.97.1 or newer, then clone the repository:

```sh
git clone https://github.com/NorviaLabs/forge.git
cd forge
cargo build --workspace --locked
```

Forge is a Rust workspace. The main binary is provided by `crates/forge-cli`, the terminal interface is in `crates/forge-tui`, and the agent loop is in `crates/forge-core`. Design documents live under `docs/designs/`.

## Making changes

1. Create a branch from the latest `main`.
2. Keep the change focused; avoid unrelated refactors or dependency updates.
3. Follow the existing code and test style in the crate you modify.
4. Add or update tests for behavior changes.
5. Update user or design documentation when commands, configuration, architecture, or safety behavior changes.
6. Never commit API keys, OAuth tokens, credentials, `.forge/` runtime data, or proprietary test fixtures.

When testing Forge itself, use a disposable repository and prefer `forge --worktree`. Worktree isolation reduces accidental branch edits but is not a security boundary. See the [alpha testing guide](docs/alpha-testing.md).

## Validation

Run focused tests while developing, then run the same checks required by CI before submitting:

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets --locked
```

For a release-sensitive CLI change, also verify the optimized binary:

```sh
cargo build --release --locked --package forge-cli
./target/release/forge --version
```

Do not silence warnings or weaken tests solely to make CI pass. If a failure is unrelated to your change, describe it clearly in the pull request.

## Pull requests

A good pull request:

- explains the problem and why the proposed approach fits Forge;
- describes user-visible and safety implications;
- lists the validation performed;
- links related issues or design documents;
- includes screenshots for meaningful TUI changes;
- avoids secrets, sensitive logs, and proprietary repository content.

Maintainers may request that broad changes be split into smaller pull requests. Reviews prioritize correctness, safe tool behavior, durable execution, clear user experience, and maintainability.

## Commit guidance

Use concise, imperative commit subjects, such as `Fix parallel credential-store tests` or `Document provider setup`. Keep commits understandable and avoid generated build artifacts.

By contributing, you agree that your contribution is licensed under the repository's [MIT License](LICENSE).
