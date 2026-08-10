#!/usr/bin/env bash
# Who may run which mc command, against the real installed tree.
#
# Integration, and deliberately not sandboxed: the thing under test IS the
# ownership and mode of /opt/minecraft, /etc/minecraft and the files the
# postinst puts there. A sandbox with test-owned copies would assert the guard
# and prove nothing about whether the group can actually reach the files.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../../lib/assert.sh"

MEMBER=mcmember      # in the minecraft group
OUTSIDER=mcoutsider  # not in it

useradd -M -s /bin/bash -G minecraft "$MEMBER"  2>/dev/null || true
useradd -M -s /bin/bash             "$OUTSIDER" 2>/dev/null || true

# A fake install, owned and moded exactly as the postinst leaves a real one.
install -o minecraft -g minecraft -m 0750 -d /opt/minecraft
: > /opt/minecraft/server.jar
chown minecraft:minecraft /opt/minecraft/server.jar
printf 'server-port=25700\nenable-rcon=true\nrcon.port=25710\nrcon.password=s3cret\n' \
    > /opt/minecraft/server.properties
chown minecraft:minecraft /opt/minecraft/server.properties
chmod 640 /opt/minecraft/server.properties
printf 's3cret\n' > /etc/minecraft/server.passwd
chown root:minecraft /etc/minecraft/server.passwd
chmod 640 /etc/minecraft/server.passwd

as() { runuser -u "$1" -- "${@:2}"; }

# ── The identities are what we think they are ────────────────────────────────
#
# Asserted rather than assumed: if runuser dropped the supplementary groups, or
# a useradd silently failed, every check below would still "pass" while testing
# nothing.

section 'Test identities'
check 'member is not root'        'no'  "$(as "$MEMBER" id -u | grep -qx 0 && echo yes || echo no)"
check 'member is in the group'    'yes' "$(as "$MEMBER" id -nG | tr ' ' '\n' | grep -qx minecraft && echo yes || echo no)"
check 'outsider is not root'      'no'  "$(as "$OUTSIDER" id -u | grep -qx 0 && echo yes || echo no)"
check 'outsider is not in the group' 'no' "$(as "$OUTSIDER" id -nG | tr ' ' '\n' | grep -qx minecraft && echo yes || echo no)"

# ── The guard itself ─────────────────────────────────────────────────────────

section 'require_root_or_group'

# lib.sh is what mc runs under, so the probe runs under the same shell options —
# `[[ ... ]]` returning non-zero inside the guard must not abort via set -e.
cat > /tmp/guard.sh <<'EOF'
set -euo pipefail
source /usr/lib/mc/lib.sh
require_root_or_group
echo ALLOWED
EOF
chmod 644 /tmp/guard.sh

bash /tmp/guard.sh > /tmp/guard.root.out 2>&1
check 'root allowed'         '0' "$?"
check_has 'root reached the end' /tmp/guard.root.out 'ALLOWED'

as "$MEMBER" bash /tmp/guard.sh > /tmp/guard.member.out 2>&1
check 'group member allowed' '0' "$?"
check_has 'member reached the end' /tmp/guard.member.out 'ALLOWED'

as "$OUTSIDER" bash /tmp/guard.sh > /tmp/guard.outsider.out 2>&1
check 'outsider refused'     '1' "$?"
check_lacks 'outsider did not reach the end' /tmp/guard.outsider.out 'ALLOWED'
check_has 'refusal names the group'  /tmp/guard.outsider.out "member of the 'minecraft' group"
check_has 'refusal says how to fix'  /tmp/guard.outsider.out 'usermod -aG minecraft'

# ── The commands ─────────────────────────────────────────────────────────────

section 'mc rcon status is open to the group'
as "$MEMBER" mc rcon status > /tmp/status.member.out 2>&1
check 'exits 0' '0' "$?"
# Proves the group actually read 0640 minecraft:minecraft server.properties —
# not merely that it got past the guard.
check_has 'read enable-rcon from server.properties' /tmp/status.member.out 'enable-rcon: true'
check_has 'read rcon.port from server.properties'   /tmp/status.member.out 'port 25710'
check_lacks 'no password/properties mismatch'       /tmp/status.member.out 'disagrees with'

section 'mc rcon status is closed to everyone else'
as "$OUTSIDER" mc rcon status > /tmp/status.outsider.out 2>&1
check 'exits 1' '1' "$?"
check_has 'refused for access, not for a missing server' /tmp/status.outsider.out 'must be run as root'
# The regression this replaces: an unreadable /opt/minecraft made server_installed
# report a server that is plainly there as absent.
check_lacks 'does not claim the server is missing' /tmp/status.outsider.out 'No server installed'

section 'writes still require root'
for u in "$MEMBER" "$OUTSIDER"; do
    as "$u" mc rcon enable > "/tmp/enable.$u.out" 2>&1
    check "rcon enable refused for $u" '1' "$?"
    check_has "refusal for $u is the root one" "/tmp/enable.$u.out" 'must be run as root.'
done

section 'an interactive session is open to the group'
# cmd_rcon refuses to connect unless the unit is active, and the image's stub
# answers is-active with 1. Override it so the run gets past that gate to the
# part under test — this one logs nothing, so it needs no writable log.
mkdir -p /tmp/bin
printf '#!/bin/bash\nexit 0\n' > /tmp/bin/systemctl
chmod 755 /tmp/bin/systemctl

# No server is listening, so this exercises the guards and then fails to
# connect. Reaching a connection error is the proof: it means the group read the
# port out of server.properties and the password out of server.passwd.
as "$MEMBER" env PATH="/tmp/bin:/usr/local/bin:/usr/bin:/bin" \
    mc rcon list > /tmp/session.member.out 2>&1
check 'exits 1 (nothing listening)' '1' "$?"
check_has 'got as far as connecting' /tmp/session.member.out 'Could not connect to 127.0.0.1:25710'
check_lacks 'not refused for access' /tmp/session.member.out 'must be run as root'

report
