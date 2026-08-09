#!/usr/bin/env bash
# Shared assertions and fixtures for tests/suites/*.sh.
#
# Every suite sources this, calls check*/section as it goes, and ends with
# `report`, whose exit status is the suite's. Suites are plain bash scripts with
# no framework: they run standalone (`bash tests/suites/unit/ports.sh`) as well
# as under tests/run.sh.

_MC_PASS=0
_MC_FAIL=0

# tests/lib/assert.sh → repo root
MC_REPO="${MC_REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
MC_PKG="$MC_REPO/packages/mc-server"
MC_RCON_PKG="$MC_REPO/packages/mc-rcon"
MC_LIB="$MC_PKG/usr/lib/mc/lib.sh"
MC_COMMON="$MC_PKG/usr/lib/mc/common.sh"

section() { printf '  %s\n' "$*"; }

check() { # check <label> <expected> <actual>
    if [[ "$2" == "$3" ]]; then
        printf '    ok   %-46s %s\n' "$1" "$3"; _MC_PASS=$((_MC_PASS + 1))
    else
        printf '    FAIL %-46s expected [%s] got [%s]\n' "$1" "$2" "$3"; _MC_FAIL=$((_MC_FAIL + 1))
    fi
}

check_has() { # check_has <label> <file> <fixed-string>
    if grep -qF -- "$3" "$2" 2>/dev/null; then
        printf '    ok   %s\n' "$1"; _MC_PASS=$((_MC_PASS + 1))
    else
        printf '    FAIL %s (missing: %s)\n' "$1" "$3"; _MC_FAIL=$((_MC_FAIL + 1))
    fi
}

check_lacks() { # check_lacks <label> <file> <fixed-string>
    if grep -qF -- "$3" "$2" 2>/dev/null; then
        printf '    FAIL %s (unexpected: %s)\n' "$1" "$3"; _MC_FAIL=$((_MC_FAIL + 1))
    else
        printf '    ok   %s\n' "$1"; _MC_PASS=$((_MC_PASS + 1))
    fi
}

check_count() { # check_count <label> <file> <fixed-string> <expected-count>
    local n; n=$(grep -cF -- "$3" "$2" 2>/dev/null || true)
    check "$1" "$4" "${n:-0}"
}

report() {
    printf '  → passed %d, failed %d\n' "$_MC_PASS" "$_MC_FAIL"
    [[ "$_MC_FAIL" -eq 0 ]]
}

# ── Fixtures ──────────────────────────────────────────────────────────────────

# Print one commented section of lib.sh, for `eval`. lib.sh cannot be sourced
# whole in a unit test: its first statement is `source /usr/lib/mc/common.sh`,
# an absolute path that only exists once the package is installed.
#   eval "$(lib_section 'Cleanup registry' 'Process lock')"
lib_section() { sed -n "/^# ── $1/,/^# ── $2/p" "$MC_LIB"; }

# Create a throwaway tree and repoint every path global at it. common.sh assigns
# these at source time, so they can only be overridden afterwards.
# Sets SANDBOX; the caller is responsible for `rm -rf "$SANDBOX"` (or a trap).
sandbox_init() {
    SANDBOX=$(mktemp -d)
    MC_BASE="$SANDBOX/opt"
    MC_CONFIG="$SANDBOX/etc"
    DEFAULTS_CONF="$MC_CONFIG/defaults.conf"
    SERVER_CONF="$MC_CONFIG/server.conf"
    PASSWD_FILE="$MC_CONFIG/server.passwd"
    MRPACK_MANIFEST="$MC_CONFIG/server.mrpack.json"
    mkdir -p "$MC_BASE" "$MC_CONFIG"
}

# GNU vs BSD stat. The suites run on Debian, but one invoked directly on a
# non-GNU userland should fail on an assertion, not on the command that
# gathers it.
file_mode() { stat -c '%a' "$1" 2>/dev/null || stat -f '%OLp' "$1"; }
