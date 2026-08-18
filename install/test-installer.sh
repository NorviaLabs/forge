#!/bin/sh
# Exercises install_sandbox_dependencies from install/forge-installer.sh across
# every branch, using PATH stubs. Each case runs in a fresh shell so that
# command lookup caching cannot leak between them.

set -u

SCRIPT="${1:-$(dirname "$0")/forge-installer.sh}"
[ -r "$SCRIPT" ] || { printf 'cannot read %s\n' "$SCRIPT" >&2; exit 1; }
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT INT TERM

# Everything above the main marker: the function definitions, safe to source.
awk '/^# --- main ---$/ { exit } { print }' "$SCRIPT" > "$WORK/lib.sh"

# A system PATH with every binary this suite simulates deliberately removed.
#
# The cases ran with /bin:/usr/bin on PATH, so whether a "missing" dependency
# was actually missing depended on the host. Two ways that broke:
#
#   - `command -v bwrap` succeeded wherever bubblewrap and socat are really
#     installed, so the installer short-circuited and the "dependency is
#     missing" cases asserted against a branch that never ran. CI installs both
#     in the step immediately before this suite.
#   - `command -v apt-get` succeeded on any Debian-family host, so the pacman,
#     apk, and no-package-manager cases silently took the apt branch instead.
#
# The suite was green where it tested nothing (18/18 on a macOS dev box) and
# red where it mattered (8/18 on Ubuntu CI). Absence has to be something the
# harness controls rather than something the host decides, so build a sanitized
# mirror of the system path once; a case that wants one of these present
# supplies it through its own stub dir, which precedes this on PATH.
SYSBIN="$WORK/sysbin"
mkdir -p "$SYSBIN"
for dir in /bin /usr/bin; do
    [ -d "$dir" ] || continue
    for path in "$dir"/*; do
        [ -x "$path" ] || continue
        base="${path##*/}"
        case "$base" in
            bwrap | socat | apt-get | pacman | apk) continue ;;
        esac
        [ -e "$SYSBIN/$base" ] || ln -s "$path" "$SYSBIN/$base"
    done
done

PASS=0
FAIL=0

# run <name> <setup-commands> -- asserts on captured output via $OUT/$RC
run_case() {
    name="$1"
    setup="$2"
    expect="$3"
    reject="$4"

    bin="$WORK/bin.$$.$(echo "$name" | tr -c 'a-zA-Z0-9' '_')"
    mkdir -p "$bin"
    log="$bin/calls.log"
    : > "$log"

    # A sudo that records that it was used and then runs the real command.
    printf '#!/bin/sh\necho "sudo $*" >> "%s"\nexec "$@"\n' "$log" > "$bin/sudo"
    chmod 755 "$bin/sudo"

    out="$( CASE_BIN="$bin" CASE_LOG="$log" SETUP="$setup" LIB="$WORK/lib.sh" \
        SYSBIN="$SYSBIN" \
        /bin/sh -c '
            PATH="$CASE_BIN:$SYSBIN"
            export PATH
            OS=Linux
            SKIP_DEPS=""
            eval "$SETUP"
            . "$LIB"
            install_sandbox_dependencies
            echo "RC=$?"
        ' 2>&1 )"

    calls="$(cat "$log" 2>/dev/null || true)"
    combined="$out
$calls"

    ok=1
    if [ -n "$expect" ] && ! printf '%s' "$combined" | grep -qF "$expect"; then
        ok=0
        why="expected to find: $expect"
    fi
    if [ $ok -eq 1 ] && [ -n "$reject" ] && printf '%s' "$combined" | grep -qF "$reject"; then
        ok=0
        why="expected NOT to find: $reject"
    fi

    if [ $ok -eq 1 ]; then
        PASS=$((PASS + 1))
        printf 'ok   %s\n' "$name"
    else
        FAIL=$((FAIL + 1))
        printf 'FAIL %s\n     %s\n--- output ---\n%s\n--------------\n' "$name" "$why" "$combined"
    fi
}

# Helpers usable inside a case's setup string.
STUB='stub() { printf "#!/bin/sh\n%s\n" "${2:-exit 0}" > "$CASE_BIN/$1"; chmod 755 "$CASE_BIN/$1"; }'

run_case "macos needs nothing" \
    "$STUB; OS=Darwin; stub apt-get 'echo apt-get \"\$@\" >> \"\$CASE_LOG\"'" \
    "RC=0" "apt-get"

run_case "skip flag is honoured" \
    "$STUB; FORGE_SKIP_DEPS=1; export FORGE_SKIP_DEPS; stub apt-get 'echo apt-get \"\$@\" >> \"\$CASE_LOG\"'" \
    "RC=0" "apt-get"

run_case "both present is a no-op" \
    "$STUB; stub bwrap; stub socat; stub apt-get 'echo apt-get \"\$@\" >> \"\$CASE_LOG\"'" \
    "RC=0" "apt-get"

run_case "apt installs both when both missing" \
    "$STUB; stub apt-get 'echo apt-get \"\$@\" >> \"\$CASE_LOG\"; printf \"#!/bin/sh\\nexit 0\\n\" > \"\$CASE_BIN/bwrap\"; printf \"#!/bin/sh\\nexit 0\\n\" > \"\$CASE_BIN/socat\"; chmod 755 \"\$CASE_BIN/bwrap\" \"\$CASE_BIN/socat\"'" \
    "apt-get install -y bubblewrap socat" ""

run_case "apt install runs under sudo" \
    "$STUB; stub apt-get 'echo apt-get \"\$@\" >> \"\$CASE_LOG\"; printf \"#!/bin/sh\\nexit 0\\n\" > \"\$CASE_BIN/bwrap\"; printf \"#!/bin/sh\\nexit 0\\n\" > \"\$CASE_BIN/socat\"; chmod 755 \"\$CASE_BIN/bwrap\" \"\$CASE_BIN/socat\"'" \
    "sudo sh -c" ""

run_case "only the missing package is installed" \
    "$STUB; stub bwrap; stub apt-get 'echo apt-get \"\$@\" >> \"\$CASE_LOG\"; printf \"#!/bin/sh\\nexit 0\\n\" > \"\$CASE_BIN/socat\"; chmod 755 \"\$CASE_BIN/socat\"'" \
    "install -y socat" "bubblewrap"

run_case "pacman is used when apt is absent" \
    "$STUB; stub pacman 'echo pacman \"\$@\" >> \"\$CASE_LOG\"; printf \"#!/bin/sh\\nexit 0\\n\" > \"\$CASE_BIN/bwrap\"; printf \"#!/bin/sh\\nexit 0\\n\" > \"\$CASE_BIN/socat\"; chmod 755 \"\$CASE_BIN/bwrap\" \"\$CASE_BIN/socat\"'" \
    "pacman -Sy --noconfirm bubblewrap socat" ""

run_case "apk is used when apt and pacman are absent" \
    "$STUB; stub apk 'echo apk \"\$@\" >> \"\$CASE_LOG\"; printf \"#!/bin/sh\\nexit 0\\n\" > \"\$CASE_BIN/bwrap\"; printf \"#!/bin/sh\\nexit 0\\n\" > \"\$CASE_BIN/socat\"; chmod 755 \"\$CASE_BIN/bwrap\" \"\$CASE_BIN/socat\"'" \
    "apk add --no-cache bubblewrap socat" ""

run_case "no package manager warns without failing" \
    "$STUB" \
    "no supported package manager" ""

run_case "no package manager still returns success" \
    "$STUB" \
    "RC=0" ""

run_case "failed install warns without failing" \
    "$STUB; stub apt-get 'exit 1'" \
    "installing them failed" ""

run_case "failed install still returns success" \
    "$STUB; stub apt-get 'exit 1'" \
    "RC=0" ""

run_case "install that does not deliver the binaries is caught" \
    "$STUB; stub apt-get 'exit 0'" \
    "still not on PATH after installing" ""

run_case "a silent no-op install never claims success" \
    "$STUB; stub apt-get 'exit 0'" \
    "" "Sandbox dependencies installed."

# PATH is narrowed to the stub dir only, so the real /usr/bin/sudo cannot be
# found. Everything the function needs from here on is a shell builtin.
run_case "missing sudo warns without failing" \
    "$STUB; stub apt-get 'exit 0'; rm -f \"\$CASE_BIN/sudo\"; id() { echo 1000; }; PATH=\"\$CASE_BIN\"" \
    "sudo is unavailable" ""

run_case "missing sudo still returns success" \
    "$STUB; stub apt-get 'exit 0'; rm -f \"\$CASE_BIN/sudo\"; id() { echo 1000; }; PATH=\"\$CASE_BIN\"" \
    "RC=0" ""

run_case "root installs without sudo" \
    "$STUB; id() { echo 0; }; stub apt-get 'echo apt-get \"\$@\" >> \"\$CASE_LOG\"; printf \"#!/bin/sh\\nexit 0\\n\" > \"\$CASE_BIN/bwrap\"; printf \"#!/bin/sh\\nexit 0\\n\" > \"\$CASE_BIN/socat\"; chmod 755 \"\$CASE_BIN/bwrap\" \"\$CASE_BIN/socat\"'" \
    "apt-get install -y bubblewrap socat" "sudo sh -c"

run_case "the warning always says what to install" \
    "$STUB" \
    "bubblewrap socat" ""

printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
