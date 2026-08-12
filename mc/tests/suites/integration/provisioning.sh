#!/usr/bin/env bash
# What `mc rcon enable` and `mc mgmt enable` leave in server.properties.
#
# The values themselves — the offsets, the secret's alphabet, the no-op second
# run — are tier-1 cargo tests against a temp root. Tier 4 because what is left
# is the part a temp root cannot have: a real root writing a file owned by a
# real service account, which is the only place the mode and owner are true or
# false. That file now holds TWO secrets, so a root-owned or world-readable
# copy is worse than it was.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../../lib/assert.sh"

install_all

# A server as the postinst leaves one, on a non-default game port so the two
# offsets are visibly derived rather than coincidentally equal to the stock
# ports.
install -o minecraft -g minecraft -m 0750 -d /opt/minecraft
printf 'server-port=25700\n' > /opt/minecraft/server.properties
chown minecraft:minecraft /opt/minecraft/server.properties
chmod 640 /opt/minecraft/server.properties

prop() { # prop <key>
    grep -E "^$1=" /opt/minecraft/server.properties | head -1 | cut -d= -f2-
}

section 'mc mgmt enable switches the protocol on'
check_true 'mc mgmt enable succeeds as root'   mc mgmt enable
check 'the protocol is enabled'   'true'      "$(prop management-server-enabled)"
# Loopback and no TLS: the endpoint authenticates every connection, and a
# keystore is not something an install should have to generate.
check 'it binds loopback'         'localhost' "$(prop management-server-host)"
check 'TLS is off for a loopback endpoint' 'false' "$(prop management-server-tls-enabled)"

section 'The port is derived from the game port, clear of RCON'
# game + 20. RCON already owns + 10, and two services that pick the same port
# fail to bind with no clue as to which one lost.
check 'the management port is the game port + 20' '25720' "$(prop management-server-port)"

section 'The secret is generated, and has the shape the protocol specifies'
secret="$(prop management-server-secret)"
check 'the secret is 40 characters' '40' "${#secret}"
check 'the secret is alphanumeric'  'yes' \
    "$([[ "$secret" =~ ^[A-Za-z0-9]{40}$ ]] && echo yes || echo no)"

section 'Re-enabling does not invalidate a client that holds the secret'
mc mgmt enable >/dev/null 2>&1
check 'the secret is unchanged' "$secret" "$(prop management-server-secret)"

section 'server.properties survives provisioning correctly owned'
# 0640 is only safe because the owner is the service account. Root-owned, the
# JVM can neither read nor write it, comes up on compiled-in defaults, and
# generates a stray world beside the real one — a failure with no error.
check 'mode is 640'  '640'                 "$(file_mode  /opt/minecraft/server.properties)"
check 'owner is the service account' 'minecraft:minecraft' \
    "$(file_owner /opt/minecraft/server.properties)"

section 'Both consoles can be provisioned into the same file'
# Neither plugin knows about the other's keys, and both rewrite the whole file.
check_true 'mc rcon enable succeeds as root' mc rcon enable
check 'RCON is enabled'                'true'  "$(prop enable-rcon)"
check 'the RCON port is the game port + 10' '25710' "$(prop rcon.port)"
check 'the management secret survived the other plugin' "$secret" \
    "$(prop management-server-secret)"
check 'the management protocol is still enabled' 'true' "$(prop management-server-enabled)"
check 'the game port was not disturbed' '25700' "$(prop server-port)"

section 'A file holding two secrets is still 0640 minecraft:minecraft'
check 'mode is 640'  '640'                 "$(file_mode  /opt/minecraft/server.properties)"
check 'owner is the service account' 'minecraft:minecraft' \
    "$(file_owner /opt/minecraft/server.properties)"
# Neither secret may leak through a world-readable copy.
check 'no other copy is readable by the world' '0' \
    "$(find /opt/minecraft -maxdepth 1 -name 'server.properties*' -perm /004 | wc -l | tr -d ' ')"

section 'Disabling leaves the secret so re-enabling restores the same endpoint'
check_true 'mc mgmt disable succeeds as root' mc mgmt disable
check 'the protocol is off'      'false'  "$(prop management-server-enabled)"
check 'the secret is still there' "$secret" "$(prop management-server-secret)"

report
