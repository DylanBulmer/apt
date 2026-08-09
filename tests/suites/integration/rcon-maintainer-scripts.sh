#!/usr/bin/env bash
# mc-rcon's postinst and prerm, driven against sandbox paths.
#
# Integration rather than unit: the scripts source /usr/lib/mc/lib.sh, so the
# package has to be installed. They are shimmed to a lib.sh that loads the real
# one and then repoints the path globals, which is the only way to exercise them
# without touching a real /opt/minecraft.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../../lib/assert.sh"

SANDBOX=$(mktemp -d)
trap 'rm -rf "$SANDBOX"' EXIT
mkdir -p "$SANDBOX/opt" "$SANDBOX/etc"

cat > "$SANDBOX/lib.sh" <<EOF
source /usr/lib/mc/lib.sh
MC_BASE="$SANDBOX/opt"
MC_CONFIG="$SANDBOX/etc"
DEFAULTS_CONF="\$MC_CONFIG/defaults.conf"
SERVER_CONF="\$MC_CONFIG/server.conf"
PASSWD_FILE="\$MC_CONFIG/server.passwd"
MC_USER="$(id -un)"
EOF
shim() { sed "s|^LIB_SH=.*|LIB_SH=\"$SANDBOX/lib.sh\"|" "$1"; }
shim "$MC_RCON_PKG/DEBIAN/postinst" > "$SANDBOX/postinst"
shim "$MC_RCON_PKG/DEBIAN/prerm"    > "$SANDBOX/prerm"

PROPS="$SANDBOX/opt/server.properties"
PW="$SANDBOX/etc/server.passwd"
getprop() { grep -m1 -- "^${1//./\\.}=" "$PROPS" 2>/dev/null | cut -d= -f2-; }

# A systemctl that reports the server as RUNNING. The default stub answers
# is-active with 1 — correct for a container, but it makes the restart branch
# unreachable, and "did this avoid an unnecessary restart" is only meaningful
# when a necessary one would have happened.
mkdir -p "$SANDBOX/bin"
cat > "$SANDBOX/bin/systemctl" <<'EOF'
#!/bin/bash
echo "$@" >> "${SYSTEMCTL_LOG:-/tmp/sc.log}"
exit 0
EOF
chmod 755 "$SANDBOX/bin/systemctl"

run() { # run <tag> <script> [dpkg-arg]
    SYSTEMCTL_LOG="$SANDBOX/$1.log" PATH="$SANDBOX/bin:$PATH" \
        bash "$SANDBOX/$2" "${3:-configure}" > "$SANDBOX/$1.out" 2>&1
}

section "postinst provisions the password even with no server yet"
# It used to bail out before this, which is why installing the plugin first and
# creating a server second left RCON off entirely.
rm -f "$PW" "$SANDBOX/etc/server.conf" "$PROPS"
run p0 postinst
check "password created"        yes "$([[ -s "$PW" ]] && echo yes)"
check "mode 0640"               640 "$(file_mode "$PW")"
check "no properties invented"  absent "$([[ -f "$PROPS" ]] && echo present || echo absent)"

section "with a server present it enables RCON"
: > "$SANDBOX/etc/server.conf"
printf 'server-port=25700\n' > "$PROPS"
run p1 postinst
check "enable-rcon"   true  "$(getprop enable-rcon)"
check "rcon.port"     25710 "$(getprop rcon.port)"
check "password written from the file" "$(cat "$PW")" "$(getprop rcon.password)"
check_has "restart requested on a real change" "$SANDBOX/p1.log" "restart minecraft"

section "re-running changes nothing and does NOT restart"
# An unattended apt upgrade must not cost a five-minute countdown for a no-op.
run p2 postinst
check_lacks "no restart" "$SANDBOX/p2.log" "restart minecraft"

section "an operator's rcon.port survives"
printf 'server-port=25700\nenable-rcon=true\nrcon.port=27000\nrcon.password=%s\n' "$(cat "$PW")" > "$PROPS"
run p3 postinst
check "rcon.port preserved"   27000 "$(getprop rcon.port)"
check "server-port untouched" 25700 "$(getprop server-port)"
check_lacks "no restart"      "$SANDBOX/p3.log" "restart minecraft"

section "prerm disables RCON and clears the secret from server.properties"
run r1 prerm remove
check "enable-rcon"          false "$(getprop enable-rcon)"
check "password cleared"     ""    "$(getprop rcon.password)"
check "password FILE kept"   yes   "$([[ -s "$PW" ]] && echo yes)"
check_has "restart requested" "$SANDBOX/r1.log" "restart minecraft"

section "re-running prerm is a no-op"
run r2 prerm remove
check_lacks "no restart" "$SANDBOX/r2.log" "restart minecraft"

section "postinst re-enables with the SAME password"
before=$(cat "$PW")
run p4 postinst
check "password unchanged"  "$before" "$(cat "$PW")"
check "properties restored" "$before" "$(getprop rcon.password)"
check "enable-rcon"         true      "$(getprop enable-rcon)"

report
