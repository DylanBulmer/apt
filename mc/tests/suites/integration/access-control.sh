#!/usr/bin/env bash
# Who may run which command, against the real installed tree.
#
# Deliberately not sandboxed: the thing under test IS the ownership and mode of
# /opt/minecraft, /etc/minecraft and the files the postinst puts there. A
# sandbox with test-owned copies would assert the guard and prove nothing about
# whether the group can actually reach the files.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../../lib/assert.sh"

install_all

MEMBER=mcmember      # in the minecraft group
OUTSIDER=mcoutsider  # not in it

useradd -M -s /bin/bash -G minecraft "$MEMBER"  2>/dev/null || true
useradd -M -s /bin/bash              "$OUTSIDER" 2>/dev/null || true

# A fake install, owned and moded exactly as the postinst leaves a real one.
install -o minecraft -g minecraft -m 0750 -d /opt/minecraft
: > /opt/minecraft/server.jar
printf 'server-port=25700\nenable-rcon=true\nrcon.port=25710\nrcon.password=s3cret\n' \
    > /opt/minecraft/server.properties
chown minecraft:minecraft /opt/minecraft/server.jar /opt/minecraft/server.properties
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
check 'member is not root'           'no'  "$(as "$MEMBER"   id -u | grep -qx 0 && echo yes || echo no)"
check 'member is in the group'       'yes' "$(as "$MEMBER"   id -nG | tr ' ' '\n' | grep -qx minecraft && echo yes || echo no)"
check 'outsider is not root'         'no'  "$(as "$OUTSIDER" id -u | grep -qx 0 && echo yes || echo no)"
check 'outsider is not in the group' 'no'  "$(as "$OUTSIDER" id -nG | tr ' ' '\n' | grep -qx minecraft && echo yes || echo no)"

# ── Read-only commands ───────────────────────────────────────────────────────
#
# The group is already the unit of access to these files: MC_BASE is 0750
# minecraft:minecraft, server.properties inside it 0640, and the RCON password
# 0640 root:minecraft. A member can therefore drive the server with
# /usr/bin/rcon by hand, so demanding root for a command that reads the same
# files would protect nothing.
section 'Read-only commands are reachable by the service group'
for cmd in status plugins; do
    check_true  "root may run mc $cmd"      mc $cmd
    check_true  "a member may run mc $cmd"  as "$MEMBER" mc $cmd
done

section 'An outsider is refused, and told how to fix it'
# sudo is installed, but there is no terminal here, so elevation is refused and
# the message is what the operator sees.
check_output 'refusal names the group' "minecraft" as "$OUTSIDER" mc status
check_output 'refusal offers usermod'  "usermod -aG minecraft" as "$OUTSIDER" mc status

section 'Mutating commands require root'
for cmd in "install --accept-eula --yes" "delete" "backup"; do
    check_output "mc $cmd refuses a member"   "must be run as root" as "$MEMBER"   mc $cmd
    check_output "mc $cmd refuses an outsider" "must be run as root" as "$OUTSIDER" mc $cmd
done

section 'Plugin subcommands enforce their own guard'
# Core cannot know whether a plugin subcommand reads or writes, so each plugin
# applies the right guard itself.
check_output 'mc rcon enable refuses a member' 'must be run as root' as "$MEMBER" mc rcon enable
check_true   'mc rcon status is allowed for a member' as "$MEMBER" mc rcon status

section 'The service account can read what it must, and no more'
check_true  'minecraft can read server.properties' \
    as minecraft test -r /opt/minecraft/server.properties
check_true  'minecraft can write server.properties' \
    as minecraft test -w /opt/minecraft/server.properties
check_false 'minecraft cannot read the backups' \
    as minecraft test -r /var/backups/minecraft
check_false 'an outsider cannot read the RCON password' \
    as "$OUTSIDER" test -r /etc/minecraft/server.passwd
check_true  'a group member can read the RCON password' \
    as "$MEMBER" test -r /etc/minecraft/server.passwd
check_false 'a group member cannot WRITE server.properties' \
    as "$MEMBER" test -w /opt/minecraft/server.properties

section 'The systemd exec targets run as the service account'
# The single most consequential row: a root guard on any of these means the
# unit never starts, and the failure reads as a config problem rather than a
# permission one.
#
# The property under test is NOT that they succeed — `mc reload` legitimately
# fails with no server running, and `mc serve` legitimately refuses an
# unaccepted EULA. It is that neither ever fails for lack of privilege.
refuses_for_privilege() { # refuses_for_privilege <user> <command...>
    local user="$1"; shift
    as "$user" "$@" 2>&1 | grep -q "must be run as root"
}
for cmd in serve shutdown reload; do
    check_false "mc $cmd is never refused for privilege" \
        refuses_for_privilege minecraft mc "$cmd"
done
# And the refusal `mc serve` DOES give is the config one, at exit 78.
check_output 'mc serve refuses on the EULA, not on privilege' 'EULA' as minecraft mc serve
as minecraft mc serve >/dev/null 2>&1
check 'mc serve exits 78 for an operator-fixable problem' '78' "$?"

report
