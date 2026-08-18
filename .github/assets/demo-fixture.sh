#!/bin/sh
# Rebuilds the workspace that .github/assets/demo.tape records against.
#
# The demo needs a bug that is real, obvious in a traceback, and fixable in one
# line — `mean([])` dividing by zero is all three, and `spread([])` gives the
# agent the same bug to fix at the end so the last beat is a genuine repeat of
# the one you just did by hand.
set -eu

WS="${1:-/tmp/forge-demo-ws}"
rm -rf "$WS"
mkdir -p "$WS"
cd "$WS"

cat > stats.py <<'PY'
"""Small helpers for summarising a run of samples."""


def mean(samples):
    return sum(samples) / len(samples)


def spread(samples):
    return max(samples) - min(samples)
PY

cat > report.py <<'PY'
from stats import mean, spread


def summarise(samples):
    return f"n={len(samples)} mean={mean(samples):.2f} spread={spread(samples)}"


if __name__ == "__main__":
    print(summarise([2, 4, 4, 4, 5, 5, 7, 9]))
    print(summarise([]))
PY

# Keep __pycache__ out of the explorer: running report.py mid-demo would
# otherwise add a directory to the tree between takes, so the same tape would
# record a different file list each time.
printf '__pycache__/\n' > .gitignore

git init -q -b main .
git add -A
git -c user.email=demo@example.com -c user.name=Demo commit -qm "Add stats helpers"

echo "fixture ready at $WS"
