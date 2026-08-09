#!/usr/bin/env bash
# install_server_artifact: staging → MC_BASE, for both server shapes.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../../lib/assert.sh"
source "$MC_COMMON"
eval "$(lib_section 'Cleanup registry'      'Process lock')"
eval "$(lib_section 'Staging helpers'       'mrpack installation')"
eval "$(lib_section 'Artifact installation' 'cmd_install ─')"

sandbox_init
trap 'rm -rf "$SANDBOX"' EXIT

staging_dirs() { find "$SANDBOX" -maxdepth 1 -name 'opt.staging.*' | wc -l | tr -d ' '; }

# Stubs for the two network-facing installers.
download_jar() {
    echo "JAR:$1:$2" > "$3"
    RESOLVED_VERSION="26.2"          # as if "latest" resolved
}
install_neoforge() {
    printf '#!/bin/sh\n' > "$2/run.sh"
    mkdir -p "$2/libraries/net"; echo lib > "$2/libraries/net/a.jar"
    RESOLVED_VERSION="21.1.99"
}

section "vanilla shape: a single jar moved into place"
rm -rf "$MC_BASE"; mkdir -p "$MC_BASE"
SERVER_TYPE=vanilla MINECRAFT_VERSION=latest
install_server_artifact
check "server.jar contents"        "JAR:vanilla:latest" "$(cat "$MC_BASE/server.jar")"
check "MINECRAFT_VERSION resolved" 26.2 "$MINECRAFT_VERSION"
check "staging removed"            0 "$(staging_dirs)"
check "cleanup registry drained"   0 "${#_MC_CLEANUP_DIRS[@]}"

section "neoforge shape: a whole tree merged into place"
rm -rf "$MC_BASE"; mkdir -p "$MC_BASE"
echo keepme > "$MC_BASE/server.properties"     # pre-existing file must survive the rsync
SERVER_TYPE=neoforge MINECRAFT_VERSION=latest
install_server_artifact
check "run.sh installed"           yes "$([[ -f "$MC_BASE/run.sh" ]] && echo yes)"
check "libraries/ installed"       lib "$(cat "$MC_BASE/libraries/net/a.jar")"
check "existing file untouched"    keepme "$(cat "$MC_BASE/server.properties")"
check "no stray server.jar"        "" "$(ls "$MC_BASE"/server.jar 2>/dev/null || true)"
check "MINECRAFT_VERSION resolved" 21.1.99 "$MINECRAFT_VERSION"
check "staging removed"            0 "$(staging_dirs)"
check "cleanup registry drained"   0 "${#_MC_CLEANUP_DIRS[@]}"

section "a mid-download failure leaves nothing behind"
# MUST be a separate bash process. `( ... ) || rc=$?` puts the subshell in a
# tested context, where bash disables set -e INSIDE it — the abort under test
# would never fire and this would pass vacuously.
rm -rf "$MC_BASE"; mkdir -p "$MC_BASE"
cat > "$SANDBOX/abort.sh" <<INNER
set -euo pipefail
source "$MC_COMMON"
eval "\$(sed -n '/^# ── Cleanup registry/,/^# ── Process lock/p'          "$MC_LIB")"
eval "\$(sed -n '/^# ── Staging helpers/,/^# ── mrpack installation/p'    "$MC_LIB")"
eval "\$(sed -n '/^# ── Artifact installation/,/^# ── cmd_install ─/p'    "$MC_LIB")"
MC_BASE="$MC_BASE"
download_jar() { echo boom >&2; return 1; }
SERVER_TYPE=vanilla
MINECRAFT_VERSION=latest
install_server_artifact
echo "REACHED-END"          # must never print
INNER
set +e
out=$(bash "$SANDBOX/abort.sh" 2>/dev/null); rc=$?
set -e
check "install aborts"                1  "$rc"
check "aborted before committing"     "" "$out"
check "no server.jar committed"       "" "$(ls "$MC_BASE"/server.jar 2>/dev/null || true)"
check "staging cleaned by EXIT trap"  0  "$(staging_dirs)"

report
