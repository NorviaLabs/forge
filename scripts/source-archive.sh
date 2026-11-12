#!/bin/sh
# Build a clean source zip of the current commit, for sharing the code outside git
# (for example, uploading it to a review sandbox).
#
# Usage: scripts/source-archive.sh [output-dir]
#   Writes <output-dir>/forge.zip (default output-dir: a new temporary directory).
#
# The zip contains only committed files (git archive), minus agent-guidance files, personal
# git metadata, docs and images. It never contains .git, target/ or node_modules/, and the
# script fails if the result still holds any of those, a key-like string, or the committer's
# email address or home directory path.
set -eu

REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
OUT=${1:-$(mktemp -d)}
STAGE="$OUT/forge"
ZIP="$OUT/forge.zip"

if [ -e "$STAGE" ] || [ -e "$ZIP" ]; then
  echo "error: $STAGE or $ZIP already exists; choose an empty output directory" >&2
  exit 1
fi
mkdir -p "$OUT"
git -C "$REPO" archive --prefix=forge/ HEAD | tar -x -C "$OUT"

# Agent-guidance files, personal git metadata, docs, images, and this script itself
# (its search patterns below would otherwise match themselves).
( cd "$STAGE" &&
  rm -rf AGENTS.md CLAUDE.md .mailmap docs artifacts .github/assets scripts/source-archive.sh &&
  find . -name EGM.md -delete &&
  { rmdir scripts 2>/dev/null || true; } )

# Always leave out git data, build output and dependencies, wherever they appear.
( cd "$OUT" && zip -qr forge.zip forge -x '*/.git/*' '*/target/*' '*/node_modules/*' '*.DS_Store' )

count() { grep -cE "$1" || true; }
listing=$(unzip -l "$ZIP")
email=$(git -C "$REPO" config user.email || true)

git_entries=$(printf '%s\n' "$listing" | count '/\.git/')
excluded=$(printf '%s\n' "$listing" | count 'AGENTS\.md|CLAUDE\.md|EGM\.md|\.mailmap|/docs/|/artifacts/|\.github/assets|/\.env$|/\.forge/|/target/|node_modules')
keys=$(unzip -p "$ZIP" | count 'sk-[0-9a-f]{16,}|sk-live|ghp_[A-Za-z0-9]{20,}|github_pat_|AKIA[0-9A-Z]{16}|-----BEGIN [A-Z ]*PRIVATE KEY')
personal_hits=$(unzip -p "$ZIP" | grep -acF -e "$HOME" ${email:+-e "$email"} || true)

echo "commit:              $(git -C "$REPO" log --oneline -1)"
echo ".git entries:        $git_entries"
echo "excluded files left: $excluded"
echo "key-like strings:    $keys"
echo "personal details:    $personal_hits"

if [ "$git_entries" != 0 ] || [ "$excluded" != 0 ] || [ "$keys" != 0 ] || [ "$personal_hits" != 0 ]; then
  echo "error: the archive failed a check above; $ZIP was not cleaned up so you can inspect it" >&2
  exit 1
fi
rm -rf "$STAGE"
echo "ok: $ZIP ($(du -h "$ZIP" | cut -f1))"
