#!/usr/bin/env bash
# Packaging invariants: install modes, the daemon-reload gate, and the
# cross-package version floor.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../../lib/assert.sh"

section "installed file modes"
# Shell files under /usr/lib are SOURCED, not executed, and are read by root —
# 0644 root:root. Only systemd's exec targets get the execute bit.
check "common.sh" "644" "$(file_mode /usr/lib/mc/common.sh)"
check "lib.sh"    "644" "$(file_mode /usr/lib/mc/lib.sh)"
check "start.sh"  "755" "$(file_mode /usr/lib/mc/start.sh)"
check "stop.sh"   "755" "$(file_mode /usr/lib/mc/stop.sh)"
check "mc"        "755" "$(file_mode /usr/bin/mc)"

section "postinst reloads systemd even on a DEGRADED system"
# The stub reports is-system-running as degraded. That used to be gated on as if
# it meant "systemd is absent", so a box with any unrelated failed unit silently
# skipped daemon-reload and never picked up the new unit file.
mkdir -p /run/systemd/system
export SYSTEMCTL_LOG=/tmp/sc-degraded.log; : > $SYSTEMCTL_LOG
dpkg -i /dist/mc-server_*.deb >/dev/null 2>&1
check_has   "daemon-reload issued"      $SYSTEMCTL_LOG "daemon-reload"
check_has   "backup timer enabled"      $SYSTEMCTL_LOG "enable minecraft-backup.timer"
check_lacks "server not auto-restarted" $SYSTEMCTL_LOG "restart minecraft"

section "no systemd at all: silent, and still succeeds"
rm -rf /run/systemd/system
export SYSTEMCTL_LOG=/tmp/sc-nosystemd.log; : > $SYSTEMCTL_LOG
dpkg -i /dist/mc-server_*.deb >/dev/null 2>&1; rc=$?
check "postinst succeeds" 0 "$rc"
check "no systemctl calls" 0 "$(wc -l < $SYSTEMCTL_LOG | tr -d ' ')"
mkdir -p /run/systemd/system

section "write_config reloads only when the timer schedule moves"
run_wc() { SYSTEMCTL_LOG="$2" bash -c '
    source /usr/lib/mc/common.sh
    eval "$(sed -n "/^# ── Output helpers/,/^# ── Plugin command registry/p" /usr/lib/mc/lib.sh)"
    eval "$(sed -n "/^# ── Config ─/,/^# ── Cleanup registry/p"              /usr/lib/mc/lib.sh)"
    load_config; BACKUP_SCHEDULE="'"$1"'"; write_config'; }
: > /tmp/wc1.log; run_wc daily  /tmp/wc1.log
check_has   "schedule changed -> reload" /tmp/wc1.log "daemon-reload"
: > /tmp/wc2.log; run_wc daily  /tmp/wc2.log
check_lacks "unchanged -> no reload"     /tmp/wc2.log "daemon-reload"
: > /tmp/wc3.log; run_wc weekly /tmp/wc3.log
check_has   "schedule moved -> reload"   /tmp/wc3.log "daemon-reload"

section "mc-rcon declares a VERSIONED dependency on mc-server"
# Its postinst calls shell functions out of mc-server's common.sh, so
# "mc-server is installed" is not a strong enough claim. Unversioned, dpkg
# configured it against an older library and the script died with
# "generate_rcon_password: command not found" (exit 127).
dep=$(dpkg-deb -f /dist/mc-rcon_*.deb Depends)
check "Depends is version-bounded" "yes" "$([[ "$dep" == *"mc-server (>= "* ]] && echo yes || echo no)"
floor=$(sed -E 's/.*mc-server \(>= ([^)]*)\).*/\1/' <<<"$dep")
have=$(dpkg-deb -f /dist/mc-server_*.deb Version)
# SATISFIABLE, not equal. Demanding equality would force an mc-rcon release for
# every mc-server release, and would declare a genuinely working pair — an
# unchanged mc-rcon against a newer mc-server — invalid. What actually matters
# is the next check: every function the postinst borrows exists in the common.sh
# being shipped. Raise the floor when that set grows, not on a schedule.
check "floor is satisfiable by the shipped mc-server" "yes" \
      "$(dpkg --compare-versions "$floor" le "$have" && echo yes || echo no)"

section "every function mc-rcon's postinst borrows actually exists"
missing=""
for fn in $(grep -oE '\b(mc_[a-z_]+|load_config|generate_rcon_password)\b' "$MC_RCON_PKG/DEBIAN/postinst" | sort -u); do
    grep -qE "^${fn}\(\)" /usr/lib/mc/common.sh || missing="$missing $fn"
done
check "no borrowed function is missing" "" "$missing"

report
