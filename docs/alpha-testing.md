# Forge Alpha Testing

Forge is alpha software. Expect rough edges, incomplete behavior, and breaking changes between releases. Do not use it as the only copy of important work.

Current release binaries support Apple Silicon macOS, Intel macOS, and x86_64 glibc Linux. They are not yet code-signed or notarized, so macOS may require approval in **System Settings → Privacy & Security**. Windows and other Linux architectures currently require an unsupported source build.

## Safe test setup

1. Use a small, disposable Git repository with no production credentials or sensitive data.
2. Commit or stash existing changes before starting Forge.
3. Start with worktree isolation: `forge --worktree`.
4. Review every requested approval and inspect the resulting diff before merging or copying changes.
5. Never paste API keys, OAuth tokens, proprietary prompts, or source code into issue reports.

Worktree isolation reduces risk to the checked-out branch, but it is not a security boundary. Forge can execute approved tools and contact the selected model provider.

## Suggested alpha exercise

In a disposable repository, ask Forge to make a small documented change and add or update a test. Confirm that it:

- understands the repository before editing;
- shows tool activity and requests approval where configured;
- produces a focused diff;
- runs relevant validation;
- can resume the session after Forge restarts.

## Data and credentials

- Repository-local sessions, offloaded context, progress, and worktrees are stored under `.forge/` in the workspace.
- Provider credentials and the last provider selection are stored in `forge/credentials.toml` under the operating system's user config directory (for example, `~/.config/forge/credentials.toml` on Linux and `~/Library/Application Support/forge/credentials.toml` on macOS).
- Credential files are created with user-only permissions on Unix. They are file-backed, not stored in the operating system keychain.
- Prompts and relevant repository context are sent to the provider you select. Provider retention and privacy policies apply.
- Forge does not send separate product telemetry. Provider API requests and any explicitly configured observability exporters still use the network.

## Reset or uninstall

Disconnect one provider with the TUI's `/connect disconnect <profile>` command, or remove all stored credentials and selections from the CLI with:

```sh
forge connect disconnect
```

Remove repository-local Forge state by deleting `.forge/` from the test repository after checking that it contains no worktree changes you want to keep.

If installed from a release archive, remove the `forge` binary you placed on `PATH`. If built with Cargo, run `cargo uninstall forge-cli`. Removing the user config directory also deletes all saved Forge credentials and provider selections.

## Reporting feedback

Use the repository's bug, setup, or feedback issue template. Include `forge --version`, platform, provider/model, reproduction steps, expected behavior, and sanitized logs. Report security issues privately through [GitHub Security Advisories](https://github.com/NorviaLabs/forge/security/advisories/new).
