# install

## Overview

Self-contained installer scripts for Forge. This directory is not a Cargo
package and has no build step; the scripts are executed directly.

- `forge-installer.sh`: POSIX shell installer for macOS and Linux — resolves a
  release, verifies its SHA-256, installs the `forge` binary, then installs the
  Linux sandbox dependencies.
- `forge-installer.ps1`: Windows entry point that intentionally installs
  nothing. Forge has no native Windows build; the script detects WSL2 and
  prints the supported install path, exiting non-zero either way.
- `test-installer.sh`: branch-coverage harness for
  `install_sandbox_dependencies` from `forge-installer.sh`, using PATH stubs
  and a sanitized mirror of the system path.

## Interface

`forge-installer.sh` environment inputs:

- `FORGE_REPOSITORY` (default `NorviaLabs/forge`),
- `FORGE_VERSION` (default: latest release tag from the GitHub API),
- `FORGE_INSTALL_DIR` (default `$HOME/.local/bin`),
- `FORGE_SKIP_DEPS` (non-empty skips sandbox dependency installation).

`forge-installer.sh` functions (sourceable above the `# --- main ---` marker):

- `detect_target` → sets `TARGET` for Darwin arm64/x86_64 and Linux x86_64;
- `package_install_command` → prints the first available package-manager
  install prefix (apt-get, dnf, yum, pacman, zypper, apk) or returns 1;
- `explain_missing_sandbox <reason> <packages>` → warning only;
- `install_sandbox_dependencies` → installs `bubblewrap` and `socat` on Linux
  when a required one is missing.

`forge-installer.ps1` exposes `Test-Wsl2` and always exits 1.

`test-installer.sh` takes an optional path to the installer (default:
sibling `forge-installer.sh`), prints `ok`/`FAIL` per case, and exits non-zero
if any case fails.

## Invariants

- The shell installer runs under `set -eu` and requires `curl` and `tar`
  up front; unsupported platform pairs fail with a message on stderr.
- The archive is only extracted and copied after the recorded SHA-256 matches
  the downloaded tree; a missing checksum entry or mismatch aborts the install.
- Downloads use HTTPS with TLS 1.2 minimum (`--proto '=https' --tlsv1.2`).
- The temp directory is always removed via an `EXIT INT TERM` trap.
- Sandbox dependency installation is never fatal: every failure path explains
  what is lost (forge still runs, but asks before every command) and returns
  success. Only the missing package(s) are requested.
- Installation uses `sudo` only when not running as root and `sudo` exists;
  the package command runs under a single `sh -c` so chained managers stay in
  one privilege escalation.
- Success is claimed only after re-checking that `bwrap` and `socat` are now on
  `PATH`; a no-op package install must not print "Sandbox dependencies
  installed."
- The Windows script never installs a binary and never returns success, because
  no sandbox backend exists for native Windows.
- The test harness isolates `PATH` per case (stub dir plus a sanitized system
  mirror with the simulated tools removed), so absence is controlled by the
  test rather than the host.

## Side Effects

- Network: queries the GitHub releases API and downloads the release archive
  and `SHA256SUMS` from `github.com`.
- Disk: creates a temp directory, extracts the archive, creates
  `FORGE_INSTALL_DIR`, and writes/chmods `$FORGE_INSTALL_DIR/forge`.
- System: on Linux, may run the host package manager as root/sudo to install
  `bubblewrap` and `socat`.
- `test-installer.sh` creates temp directories and stub executables, creates
  symlinks into a sanitized `sysbin`, and removes its work directory on exit.

## Verification

- Run: `sh install/test-installer.sh` (passes the installer path as `$1` to
  test a non-default copy). There is no Cargo target for this directory.
