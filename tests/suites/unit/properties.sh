#!/usr/bin/env bash
# server.properties seeding and the managed-key defence.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../../lib/assert.sh"
source "$MC_COMMON"
eval "$(lib_section 'Output helpers'            'Plugin command registry')"
eval "$(lib_section 'RCON helpers'              'server.properties helpers')"
eval "$(lib_section 'server.properties helpers' 'Download helpers')"

sandbox_init
trap 'rm -rf "$SANDBOX"' EXIT
MC_USER="$(id -un)"   # chown is tolerated-but-failing as non-root; keep modes observable
: > "$SERVER_CONF"    # exists but says nothing about ports — there is no such setting

section "fresh install, mc-rcon NOT installed"
rm -f "$PASSWD_FILE" "$MC_BASE/server.properties"
init_server_properties
check "server-port"   25565 "$(mc_sprop_get server-port)"
check "enable-rcon"   false "$(mc_sprop_get enable-rcon)"
check "rcon.port"     25575 "$(mc_sprop_get rcon.port)"
check "rcon.password" ""    "$(mc_sprop_get rcon.password)"

section "fresh install AFTER mc-rcon provisioned a password"
# Installing the plugin first used to leave RCON off despite a provisioned
# password, silently degrading the stop countdown, backups and `mc rcon`.
rm -f "$MC_BASE/server.properties"
echo "s3cret-pw" > "$PASSWD_FILE"
init_server_properties
check "enable-rcon now true" true      "$(mc_sprop_get enable-rcon)"
check "password seeded"      s3cret-pw "$(mc_sprop_get rcon.password)"

section "RCON is provisioned when mc-rcon is present but the password is not"
# mc-rcon's postinst is the only other writer of this file and it bails out when
# no server exists yet, so `apt install mc-rcon` before `mc install` — and
# `mc delete` followed by `mc install` — used to leave RCON off on a machine
# with the plugin installed.
FAKEBIN="$SANDBOX/bin"; mkdir -p "$FAKEBIN"
printf '#!/bin/sh\nexit 0\n' > "$FAKEBIN/rcon"; chmod 755 "$FAKEBIN/rcon"

rm -f "$PASSWD_FILE" "$MC_BASE/server.properties"
PATH="$FAKEBIN:$PATH" ensure_rcon_password >/dev/null
check "password generated"   yes "$([[ -s "$PASSWD_FILE" ]] && echo yes)"
check "mode 0640"            640 "$(file_mode "$PASSWD_FILE")"
check "base64url charset"    ok  "$(grep -qE '^[A-Za-z0-9_-]+$' "$PASSWD_FILE" && echo ok)"
init_server_properties
check "enable-rcon true"     true "$(mc_sprop_get enable-rcon)"
check "password matches file" "$(cat "$PASSWD_FILE")" "$(mc_sprop_get rcon.password)"

section "an existing password is never regenerated"
before=$(cat "$PASSWD_FILE")
PATH="$FAKEBIN:$PATH" ensure_rcon_password >/dev/null
check "unchanged" "$before" "$(cat "$PASSWD_FILE")"

section "without mc-rcon installed, no password is invented"
# PATH is emptied for this ONE call rather than just dropping the fake: run.sh
# installs both packages before every suite, so the real /usr/bin/rcon is on
# PATH and "mc-rcon absent" has to be simulated deliberately. `command -v` is a
# builtin, and the function returns before it needs any external command.
rm -f "$PASSWD_FILE" "$MC_BASE/server.properties"
mkdir -p "$SANDBOX/empty"
PATH="$SANDBOX/empty" ensure_rcon_password >/dev/null
check "no password file" absent "$([[ -f "$PASSWD_FILE" ]] && echo present || echo absent)"
init_server_properties
check "enable-rcon false" false "$(mc_sprop_get enable-rcon)"
echo "s3cret-pw" > "$PASSWD_FILE"        # restore the fixture for the sections below

section "a legacy SERVER_PORT in server.conf does not seed the new file"
# Older installs still carry the line. It must not leak into a freshly created
# server.properties — the stock port is the only seed.
rm -f "$MC_BASE/server.properties"
printf 'SERVER_PORT=25600\n' > "$SERVER_CONF"
init_server_properties
check "server-port is the stock port" 25565 "$(mc_sprop_get server-port)"
check "rcon.port derived from it"     25575 "$(mc_sprop_get rcon.port)"
: > "$SERVER_CONF"

section "managed_property_value does not abort on an absent key"
# It used to parse with `grep|cut`, whose non-match status (1) propagated
# through a plain assignment and killed the whole mc invocation under set -e.
printf 'server-port=25700\n' > "$MC_BASE/server.properties"
got=$(managed_property_value rcon.password)
check "absent managed key -> derived" s3cret-pw "$got"
check "still running afterwards"      yes yes

section "a hostile pack cannot seize the managed keys"
printf 'server-port=25700\nenable-rcon=true\nrcon.port=27000\nrcon.password=mine\n' \
    > "$MC_BASE/server.properties"
cat > "$SANDBOX/pack.properties" <<'EOF'
server-port=1234
enable-rcon=true
rcon.port=4321
rcon.password=attacker-chosen
motd=Modpack Server
difficulty=hard
EOF
merge_server_properties "$SANDBOX/pack.properties"
check "live server-port kept" 25700 "$(mc_sprop_get server-port)"
check "live rcon.port kept"   27000 "$(mc_sprop_get rcon.port)"
check "live password kept"    mine  "$(mc_sprop_get rcon.password)"
check "pack's motd applied"   "Modpack Server" "$(mc_sprop_get motd)"
check "pack's difficulty"     hard  "$(mc_sprop_get difficulty)"

section "merge onto a first-time install (no live file)"
rm -f "$MC_BASE/server.properties"
load_config   # cmd_install_mrpack does this before calling merge
merge_server_properties "$SANDBOX/pack.properties"
check "port from config, not pack" 25565 "$(mc_sprop_get server-port)"
check "rcon.port derived"          25575 "$(mc_sprop_get rcon.port)"
check "password from passwd file"  s3cret-pw "$(mc_sprop_get rcon.password)"
check "mode is 0640"               640 "$(file_mode "$MC_BASE/server.properties")"

report
