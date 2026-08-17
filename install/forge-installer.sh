#!/bin/sh

set -eu

REPOSITORY="${FORGE_REPOSITORY:-NorviaLabs/forge}"
VERSION="${FORGE_VERSION:-}"
INSTALL_DIR="${FORGE_INSTALL_DIR:-${HOME}/.local/bin}"
SKIP_DEPS="${FORGE_SKIP_DEPS:-}"
API_URL="https://api.github.com/repos/${REPOSITORY}/releases?per_page=20"

fail() {
    printf 'forge installer: %s\n' "$1" >&2
    exit 1
}

warn() {
    printf 'forge installer: %s\n' "$1" >&2
}

detect_target() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"
    case "${OS}:${ARCH}" in
        Darwin:arm64|Darwin:aarch64)
            TARGET="aarch64-apple-darwin"
            ;;
        Darwin:x86_64)
            TARGET="x86_64-apple-darwin"
            ;;
        Linux:x86_64|Linux:amd64)
            TARGET="x86_64-unknown-linux-gnu"
            ;;
        *)
            fail "unsupported platform: ${OS}/${ARCH}"
            ;;
    esac
}

# Prints the install command for the first package manager found, with the
# packages appended. Every distribution we support happens to name these
# packages identically, so only the command varies.
package_install_command() {
    if command -v apt-get >/dev/null 2>&1; then
        printf 'apt-get update && apt-get install -y'
    elif command -v dnf >/dev/null 2>&1; then
        printf 'dnf install -y'
    elif command -v yum >/dev/null 2>&1; then
        printf 'yum install -y'
    elif command -v pacman >/dev/null 2>&1; then
        printf 'pacman -Sy --noconfirm'
    elif command -v zypper >/dev/null 2>&1; then
        printf 'zypper --non-interactive install'
    elif command -v apk >/dev/null 2>&1; then
        printf 'apk add --no-cache'
    else
        return 1
    fi
}

# Explains what the user loses by not having the sandbox, and how to get it
# back. Never fatal: forge runs without these, it just has to ask about every
# command instead of confining it.
explain_missing_sandbox() {
    warn "$1"
    warn "forge will run, but cannot confine the commands it executes, so it"
    warn "will ask for approval on every one. To enable the sandbox, install:"
    warn "    ${2}"
}

# Linux confines agent-spawned commands with bubblewrap and relays the egress
# proxy into the sandbox's network namespace with socat. Without them forge
# silently drops to asking about every command, which is a materially different
# product — so install them rather than let the sandbox quietly disappear.
# macOS uses the built-in sandbox-exec and needs nothing.
install_sandbox_dependencies() {
    [ "$OS" = "Linux" ] || return 0
    [ -z "$SKIP_DEPS" ] || return 0

    MISSING=""
    command -v bwrap >/dev/null 2>&1 || MISSING="bubblewrap"
    command -v socat >/dev/null 2>&1 || MISSING="${MISSING:+${MISSING} }socat"
    [ -n "$MISSING" ] || return 0

    if [ "$(id -u)" = "0" ]; then
        SUDO=""
    elif command -v sudo >/dev/null 2>&1; then
        SUDO="sudo "
    else
        explain_missing_sandbox "not running as root and sudo is unavailable" "$MISSING"
        return 0
    fi

    if ! INSTALL_CMD="$(package_install_command)"; then
        explain_missing_sandbox "no supported package manager found" "$MISSING"
        return 0
    fi

    printf 'Installing sandbox dependencies (%s):\n' "$MISSING"
    printf '    %s%s %s\n' "$SUDO" "$INSTALL_CMD" "$MISSING"
    # The whole command runs under one shell so that chained package managers
    # (apt-get update && apt-get install) stay inside the single sudo.
    if ! ${SUDO}sh -c "${INSTALL_CMD} ${MISSING}"; then
        explain_missing_sandbox "installing them failed" "$MISSING"
        return 0
    fi

    STILL_MISSING=""
    command -v bwrap >/dev/null 2>&1 || STILL_MISSING="bubblewrap"
    command -v socat >/dev/null 2>&1 || STILL_MISSING="${STILL_MISSING:+${STILL_MISSING} }socat"
    if [ -n "$STILL_MISSING" ]; then
        explain_missing_sandbox "still not on PATH after installing" "$STILL_MISSING"
        return 0
    fi

    printf 'Sandbox dependencies installed.\n'
}

# --- main ---

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"

if [ -z "$VERSION" ]; then
    VERSION="$(curl -fsSL -H 'Accept: application/vnd.github+json' "$API_URL" \
        | sed -n 's/^[[:space:]]*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' \
        | head -n 1)"
fi
[ -n "$VERSION" ] || fail "could not determine the latest release; set FORGE_VERSION"

detect_target

ASSET="forge-${VERSION}-${TARGET}.tar.gz"
BASE_URL="https://github.com/${REPOSITORY}/releases/download/${VERSION}"
TEMP_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t forge-installer)"
trap 'rm -rf "$TEMP_DIR"' EXIT INT TERM

curl -fL --proto '=https' --tlsv1.2 "${BASE_URL}/${ASSET}" -o "${TEMP_DIR}/${ASSET}"
curl -fL --proto '=https' --tlsv1.2 "${BASE_URL}/SHA256SUMS" -o "${TEMP_DIR}/SHA256SUMS"

EXPECTED="$(awk -v file="$ASSET" '$2 == file { print $1; exit }' "${TEMP_DIR}/SHA256SUMS")"
[ -n "$EXPECTED" ] || fail "no checksum found for ${ASSET}"

if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL="$(sha256sum "${TEMP_DIR}/${ASSET}" | awk '{print $1}')"
else
    command -v shasum >/dev/null 2>&1 || fail "sha256sum or shasum is required"
    ACTUAL="$(shasum -a 256 "${TEMP_DIR}/${ASSET}" | awk '{print $1}')"
fi
[ "$EXPECTED" = "$ACTUAL" ] || fail "checksum verification failed"

tar -xzf "${TEMP_DIR}/${ASSET}" -C "$TEMP_DIR"
mkdir -p "$INSTALL_DIR"
cp "${TEMP_DIR}/forge-${VERSION}-${TARGET}/forge" "${INSTALL_DIR}/forge"
chmod 755 "${INSTALL_DIR}/forge"

printf 'Installed Forge %s to %s/forge\n' "$VERSION" "$INSTALL_DIR"

# After the binary is in place, so that a dependency failure never costs the
# user the download they already verified.
install_sandbox_dependencies

case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *) printf 'Add %s to PATH to run forge from any shell.\n' "$INSTALL_DIR" ;;
esac
