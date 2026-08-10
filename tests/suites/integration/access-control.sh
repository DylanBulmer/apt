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
    # Also checks the refusal echoes back the argv the dispatcher captured,
    # which is what mc_elevate would have re-run.
    check_has "refusal for $u is the root one" "/tmp/enable.$u.out" \
              'must be run as root: sudo mc rcon enable'
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

# ── Elevation ────────────────────────────────────────────────────────────────
#
# From here on the member may sudo without a password. That isolates what is
# under test — whether mc decides to escalate — from sudo's authentication,
# which is not ours to verify.
echo "$MEMBER ALL=(ALL) NOPASSWD: ALL" > /etc/sudoers.d/mc-test
chmod 440 /etc/sudoers.d/mc-test

section 'a root-only command re-runs itself under sudo'
# script(1) supplies the pty that mc_elevate requires. `mc backup` is a good
# probe: it writes into /var/backups/minecraft, which is root:root 0700, so the
# archive appearing is proof the work ran as root and not merely that the
# command exited 0.
rm -f /var/backups/minecraft/minecraft-*.tar.gz
as "$MEMBER" script -qec "mc backup" /dev/null > /tmp/elevate.out 2>&1
check 'elevated run succeeds' '0' "$?"
check_has 'the escalation is announced' /tmp/elevate.out 'needs root'
check_has 'the command then ran'        /tmp/elevate.out 'Backup complete'
check 'archive was written to the root-only dir' '1' \
      "$(ls -1 /var/backups/minecraft/minecraft-*.tar.gz 2>/dev/null | wc -l | tr -d ' ')"
# If the member could reach that directory itself, the check above would prove
# nothing about privilege.
as "$MEMBER" ls /var/backups/minecraft > /dev/null 2>&1
check 'member cannot read the backup dir unaided' '2' "$?"

section 'without a terminal it refuses rather than hanging'
# Same user, same sudo rights, no pty — the backup timer, a hook and a CI runner
# all look like this, and a password prompt there would block forever.
rm -f /var/backups/minecraft/minecraft-*.tar.gz
as "$MEMBER" mc backup > /tmp/notty.out 2>&1
check 'refused' '1' "$?"
check_has 'told how to run it'  /tmp/notty.out 'sudo mc backup'
check_lacks 'did not escalate'  /tmp/notty.out 'needs root — re-running'
check 'no archive written' '0' \
      "$(ls -1 /var/backups/minecraft/minecraft-*.tar.gz 2>/dev/null | wc -l | tr -d ' ')"

section 'with no sudo installed it degrades to a plain refusal'
# What lets mc-server leave sudo out of its Depends: the feature is optional and
# its absence must cost nothing but the convenience. The image has sudo, so the
# binary is hidden behind a stripped PATH rather than uninstalled.
#
# bash has to be reachable on that PATH too: mc is `#!/usr/bin/env bash`, and
# env resolves the interpreter through PATH, so the shebang would fail before
# mc ever ran.
mkdir -p /tmp/nosudo
ln -sf /usr/bin/mc /tmp/nosudo/mc
ln -sf /bin/bash   /tmp/nosudo/bash
as "$MEMBER" script -qec "env PATH=/tmp/nosudo mc backup" /dev/null > /tmp/nosudo.out 2>&1
check 'refused' '1' "$?"
check_lacks 'did not escalate'    /tmp/nosudo.out 'needs root — re-running'
check_has 'plain refusal instead' /tmp/nosudo.out 'must be run as root: sudo mc backup'
check 'no archive written' '0' \
      "$(ls -1 /var/backups/minecraft/minecraft-*.tar.gz 2>/dev/null | wc -l | tr -d ' ')"

section 'an already-elevated run never escalates again'
# The loop guard. A sudoers runas_default that is not root would otherwise put
# this in a sudo prompt that never ends.
as "$MEMBER" script -qec "env MC_ELEVATED=1 mc backup" /dev/null > /tmp/loop.out 2>&1
check 'refused' '1' "$?"
check_lacks 'did not escalate' /tmp/loop.out 'needs root — re-running'
check_has 'plain refusal instead' /tmp/loop.out 'must be run as root'

report
