#!/usr/bin/env bash
# mc-rcon's postinst, driven against sandbox paths by shimming common.sh.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../../lib/assert.sh"

SANDBOX=$(mktemp -d)
trap 'rm -rf "$SANDBOX"' EXIT
mkdir -p "$SANDBOX/opt" "$SANDBOX/etc"

# A common.sh that loads the real one and then repoints the path globals and the
# service account at the sandbox, so chown succeeds unprivileged.
cat > "$SANDBOX/common.sh" <<EOF
source "$MC_COMMON"
MC_BASE="$SANDBOX/opt"
MC_CONFIG="$SANDBOX/etc"
DEFAULTS_CONF="\$MC_CONFIG/defaults.conf"
SERVER_CONF="\$MC_CONFIG/server.conf"
PASSWD_FILE="\$MC_CONFIG/server.passwd"
MC_USER="$(id -un)"
EOF
sed "s|^COMMON_SH=.*|COMMON_SH=\"$SANDBOX/common.sh\"|" \
    "$MC_RCON_PKG/DEBIAN/postinst" > "$SANDBOX/postinst"

PROPS="$SANDBOX/opt/server.properties"
getprop() { grep -m1 -- "^${1//./\\.}=" "$PROPS" | cut -d= -f2-; }
echo "s3cret-pw" > "$SANDBOX/etc/server.passwd"   # pre-provisioned: skips the chown branch

section "a stale server.conf must not drag rcon.port with it"
printf 'SERVER_PORT=25565\n' > "$SANDBOX/etc/server.conf"      # stale
printf 'server-port=25700\nenable-rcon=true\nrcon.port=27000\nrcon.password=s3cret-pw\n' > "$PROPS"
SYSTEMCTL_LOG="$SANDBOX/sc1.log" bash "$SANDBOX/postinst" configure > "$SANDBOX/out1" 2>&1
check "operator's rcon.port preserved" 27000 "$(getprop rcon.port)"
check "server-port preserved"          25700 "$(getprop server-port)"
check_lacks "no restart for a no-op"   "$SANDBOX/out1" "restarting"

section "no rcon.port yet: derived from the LIVE port, not server.conf"
printf 'SERVER_PORT=25565\n' > "$SANDBOX/etc/server.conf"      # still stale
printf 'server-port=25700\n' > "$PROPS"
SYSTEMCTL_LOG="$SANDBOX/sc2.log" bash "$SANDBOX/postinst" configure >/dev/null 2>&1
check "rcon.port = live port + 10" 25710 "$(getprop rcon.port)"
check "enable-rcon turned on"      true  "$(getprop enable-rcon)"
check "password written"           s3cret-pw "$(getprop rcon.password)"

section "fresh server, conf and properties agree"
printf 'SERVER_PORT=25565\n' > "$SANDBOX/etc/server.conf"
printf 'server-port=25565\n' > "$PROPS"
SYSTEMCTL_LOG="$SANDBOX/sc3.log" bash "$SANDBOX/postinst" configure >/dev/null 2>&1
check "rcon.port" 25575 "$(getprop rcon.port)"

section "no server installed yet: exits without touching anything"
rm -f "$SANDBOX/etc/server.conf"
set +e; bash "$SANDBOX/postinst" configure >/dev/null 2>&1; rc=$?; set -e
check "postinst exit status" 0 "$rc"

report
