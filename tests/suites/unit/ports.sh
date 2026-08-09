#!/usr/bin/env bash
# Port resolution: server.properties is the source of truth, server.conf seeds.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../../lib/assert.sh"
source "$MC_COMMON"

sandbox_init
trap 'rm -rf "$SANDBOX"' EXIT

setup() { # setup <server.conf port> [properties lines...]
    printf 'SERVER_PORT=%s\n' "$1" > "$SERVER_CONF"; shift
    rm -f "$MC_BASE/server.properties"
    [[ $# -gt 0 ]] && printf '%s\n' "$@" > "$MC_BASE/server.properties"
    return 0
}

section "no server.properties: server.conf seeds both"
setup 25565; load_config
check "SERVER_PORT"   25565 "$SERVER_PORT"
check "mc_rcon_port"  25575 "$(mc_rcon_port)"
setup 25600; load_config
check "SERVER_PORT (non-stock)"  25600 "$SERVER_PORT"
check "mc_rcon_port (non-stock)" 25610 "$(mc_rcon_port)"

section "properties overrides a stale server.conf (the drift bug)"
setup 25565 "server-port=25700"; load_config
check "SERVER_PORT follows properties"  25700 "$SERVER_PORT"
check "mc_rcon_port follows properties" 25710 "$(mc_rcon_port)"

section "a hand-set rcon.port is used verbatim"
setup 25565 "server-port=25700" "rcon.port=27000"; load_config
check "mc_rcon_port = rcon.port" 27000 "$(mc_rcon_port)"

section "malformed values fall back rather than evaluate"
setup 25565 "server-port=25700" "rcon.port=abc"; load_config
check "bad rcon.port -> server-port+10" 25710 "$(mc_rcon_port)"
setup 25565 "server-port=not-a-port"; load_config
check "bad server-port -> conf"    25565 "$SERVER_PORT"
check "bad server-port -> conf+10" 25575 "$(mc_rcon_port)"

section "arithmetic-context injection is inert"
# [[ x -ge y ]] evaluates its operands as expressions, and bash performs command
# substitution inside array subscripts while doing so. Never let an unvalidated
# string reach one.
CANARY="$SANDBOX/pwned"
setup 25565 "server-port=25700" "rcon.port=PATH[\$(touch $CANARY)]"; load_config
check "injected rcon.port falls through"  25710 "$(mc_rcon_port)"
setup 25565 "server-port=PATH[\$(touch $CANARY)]"; load_config
check "injected server-port falls through" 25575 "$(mc_rcon_port)"
check "no command substitution executed" "absent" "$([[ -e "$CANARY" ]] && echo present || echo absent)"

section "mc_sprop_get never fails (callers run under set -e)"
setup 25565 "server-port=25700" "rcon.password=a=b=c" "motd=hi"
check "absent key -> empty"       "" "$(mc_sprop_get nosuchkey)"
check "value containing '='"  "a=b=c" "$(mc_sprop_get rcon.password)"
rm -f "$MC_BASE/server.properties"
check "absent file -> empty"      "" "$(mc_sprop_get server-port)"
printf 'rconXport=9999\nrcon.port=27000\n' > "$MC_BASE/server.properties"
check "'.' is not a wildcard"  27000 "$(mc_sprop_get rcon.port)"

section "no server.conf at all"
rm -f "$SERVER_CONF" "$MC_BASE/server.properties"; load_config
check "built-in default"     25565 "$SERVER_PORT"
check "built-in default +10" 25575 "$(mc_rcon_port)"

report
