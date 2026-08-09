#!/usr/bin/env bash
# server.properties seeding and the managed-key defence.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../../lib/assert.sh"
source "$MC_COMMON"
eval "$(lib_section 'server.properties helpers' 'Download helpers')"

sandbox_init
trap 'rm -rf "$SANDBOX"' EXIT
MC_USER="$(id -un)"   # chown is tolerated-but-failing as non-root; keep modes observable
printf 'SERVER_PORT=25565\n' > "$SERVER_CONF"

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

section "seeds from a non-stock port in server.conf"
rm -f "$MC_BASE/server.properties"
printf 'SERVER_PORT=25600\n' > "$SERVER_CONF"
init_server_properties
check "server-port" 25600 "$(mc_sprop_get server-port)"
check "rcon.port"   25610 "$(mc_sprop_get rcon.port)"
printf 'SERVER_PORT=25565\n' > "$SERVER_CONF"

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
