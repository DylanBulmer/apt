#!/usr/bin/env bash
# Shared assertions and fixtures for tests/suites/*.sh.
#
# Every suite sources this, calls check*/section as it goes, and ends with
# `report`, whose exit status is the suite's. Suites are plain bash scripts with
# no framework, and each installs whatever set of packages it is about — the
# plugins suite in particular needs to run with core alone.

_MC_PASS=0
_MC_FAIL=0

MC_REPO="${MC_REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
MC_DIST="${MC_DIST:-/dist}"

section() { printf '  %s\n' "$*"; }

check() { # check <label> <expected> <actual>
    if [[ "$2" == "$3" ]]; then
        printf '    ok   %-52s %s\n' "$1" "$3"; _MC_PASS=$((_MC_PASS + 1))
    else
        printf '    FAIL %-52s expected [%s] got [%s]\n' "$1" "$2" "$3"; _MC_FAIL=$((_MC_FAIL + 1))
    fi
}

check_true() { # check_true <label> <command...>
    local label="$1"; shift
    if "$@" >/dev/null 2>&1; then
        printf '    ok   %s\n' "$label"; _MC_PASS=$((_MC_PASS + 1))
    else
        printf '    FAIL %s (command failed: %s)\n' "$label" "$*"; _MC_FAIL=$((_MC_FAIL + 1))
    fi
}

check_false() { # check_false <label> <command...>
    local label="$1"; shift
    if "$@" >/dev/null 2>&1; then
        printf '    FAIL %s (command unexpectedly succeeded: %s)\n' "$label" "$*"; _MC_FAIL=$((_MC_FAIL + 1))
    else
        printf '    ok   %s\n' "$label"; _MC_PASS=$((_MC_PASS + 1))
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

check_output() { # check_output <label> <fixed-string> <command...>
    local label="$1" needle="$2"; shift 2
    local out
    out=$("$@" 2>&1 || true)
    if [[ "$out" == *"$needle"* ]]; then
        printf '    ok   %s\n' "$label"; _MC_PASS=$((_MC_PASS + 1))
    else
        printf '    FAIL %s (wanted %q in output)\n      got: %s\n' "$label" "$needle" "$out"
        _MC_FAIL=$((_MC_FAIL + 1))
    fi
}

report() {
    printf '  → passed %d, failed %d\n' "$_MC_PASS" "$_MC_FAIL"
    [[ "$_MC_FAIL" -eq 0 ]]
}

# ── Package installation ──────────────────────────────────────────────────────

# Install core. Every plugin Depends: mc-server, so this always comes first.
install_core() {
    dpkg -i "$MC_DIST"/mc-server_*.deb >/dev/null 2>&1
}

# Install one or more plugins by short name: install_plugins rcon backup
install_plugins() {
    local name debs=()
    for name in "$@"; do debs+=("$MC_DIST/mc-${name}"_*.deb); done
    dpkg -i "${debs[@]}" >/dev/null 2>&1
}

# Both consoles, deliberately: mc-rcon and mc-mgmt are designed to coexist and
# be elected between, so the default fixture is a machine that has both.
install_all() {
    install_core
    install_plugins rcon mgmt backup mrpack
}

# ── Fixtures ──────────────────────────────────────────────────────────────────

# GNU vs BSD stat. The suites run on Debian, but one invoked directly on a
# non-GNU userland should fail on an assertion, not on the command that
# gathers it.
file_mode() { stat -c '%a' "$1" 2>/dev/null || stat -f '%OLp' "$1"; }
file_owner() { stat -c '%U:%G' "$1" 2>/dev/null || stat -f '%Su:%Sg' "$1"; }
