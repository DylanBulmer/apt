#!/usr/bin/env bash
# Port resolution. Ports belong to the server, so server.properties is the only
# place they are read from — mc's own config describes how to run the server,
# not what it is. This suite is what keeps a mirror from creeping into it.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../../lib/assert.sh"
source "$MC_COMMON"

sandbox_init
trap 'rm -rf "$SANDBOX"' EXIT

props() { # props [lines...]
    rm -f "$MC_BASE/server.properties"
    [[ $# -gt 0 ]] && printf '%s\n' "$@" > "$MC_BASE/server.properties"
    return 0
}

section "no server.properties yet: the stock port stands in"
props; load_config
check "mc_rcon_port" 25575 "$(mc_rcon_port)"
check "stock port constant" 25565 "$MC_STOCK_PORT"

section "the live game port drives the RCON port"
props "server-port=25700"; load_config
check "server-port + 10" 25710 "$(mc_rcon_port)"

section "a hand-set rcon.port is used verbatim"
props "server-port=25700" "rcon.port=27000"; load_config
check "rcon.port wins over the +10 convention" 27000 "$(mc_rcon_port)"

section "mc's own config cannot set a port"
# A port-shaped variable in server.conf must never become a source of truth,
# whether someone adds one by hand or a future change reintroduces the idea.
printf 'SERVER_PORT=29999\n' > "$SERVER_CONF"
props "server-port=25700"; load_config
check "properties still wins"       25710 "$(mc_rcon_port)"
props; load_config
check "and with no properties file" 25575 "$(mc_rcon_port)"
rm -f "$SERVER_CONF"

section "malformed values fall back rather than evaluate"
props "server-port=25700" "rcon.port=abc"; load_config
check "bad rcon.port -> server-port+10" 25710 "$(mc_rcon_port)"
props "server-port=not-a-port"; load_config
check "bad server-port -> stock+10"     25575 "$(mc_rcon_port)"

section "arithmetic-context injection is inert"
# [[ x -ge y ]] evaluates its operands as expressions, and bash performs command
# substitution inside array subscripts while doing so. Never let an unvalidated
# string reach one.
CANARY="$SANDBOX/pwned"
props "server-port=25700" "rcon.port=PATH[\$(touch $CANARY)]"; load_config
check "injected rcon.port falls through"   25710 "$(mc_rcon_port)"
props "server-port=PATH[\$(touch $CANARY)]"; load_config
check "injected server-port falls through" 25575 "$(mc_rcon_port)"
check "no command substitution executed" "absent" \
      "$([[ -e "$CANARY" ]] && echo present || echo absent)"

section "mc_sprop_get never fails (callers run under set -e)"
props "server-port=25700" "rcon.password=a=b=c" "motd=hi"
check "absent key -> empty"      ""      "$(mc_sprop_get nosuchkey)"
check "value containing '='"     "a=b=c" "$(mc_sprop_get rcon.password)"
props
check "absent file -> empty"     ""      "$(mc_sprop_get server-port)"
props "rconXport=9999" "rcon.port=27000"
check "'.' is not a wildcard"    27000   "$(mc_sprop_get rcon.port)"

report
