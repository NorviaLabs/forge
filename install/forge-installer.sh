#!/bin/sh

set -eu

REPOSITORY="${FORGE_REPOSITORY:-NorviaLabs/forge}"
VERSION="${FORGE_VERSION:-}"
INSTALL_DIR="${FORGE_INSTALL_DIR:-${HOME}/.local/bin}"
API_URL="https://api.github.com/repos/${REPOSITORY}/releases?per_page=20"

fail() {
    printf 'forge installer: %s\n' "$1" >&2
    exit 1
}

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"

if [ -z "$VERSION" ]; then
    VERSION="$(curl -fsSL -H 'Accept: application/vnd.github+json' "$API_URL" \
        | sed -n 's/^[[:space:]]*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' \
        | head -n 1)"
fi
[ -n "$VERSION" ] || fail "could not determine the latest release; set FORGE_VERSION"

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
case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *) printf 'Add %s to PATH to run forge from any shell.\n' "$INSTALL_DIR" ;;
esac
