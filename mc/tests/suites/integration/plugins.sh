#!/usr/bin/env bash
# Installing and removing plugins, as an operator would.
#
# The point of the whole rewrite: adding capability is installing another .deb,
# and removing one leaves core working. Tier 4 because it is dpkg that has to
# do it — the cargo tests cover discovery against fixture manifests, but not
# whether a real package's install and removal actually land.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../../lib/assert.sh"

install_core

section 'Core alone is a working install'
check_true 'mc runs'                mc --version
check_true 'mc plugins runs'        mc plugins
check_output 'no plugins installed' 'No plugins installed' mc plugins

section 'A command core does not implement points at the package that adds it'
# Discoverable from the refusal rather than from the manual.
check_output 'mc rcon suggests mc-rcon'      'apt install mc-rcon'    mc rcon
check_output 'mc backup suggests mc-backup'  'apt install mc-backup'  mc backup
check_output 'a .mrpack suggests mc-mrpack'  'apt install mc-mrpack'  \
    mc install /tmp/nonexistent.mrpack --accept-eula

section 'Upgrade refuses without a backup provider'
# The shell version could assume a backup was always available because it lived
# in the same package. Now that mc-backup is removable, "no backup was taken"
# has to be a decision rather than a consequence.
: > /opt/minecraft/server.jar
printf 'eula=true\n' > /opt/minecraft/eula.txt
check_output 'refusal names the package' 'apt install mc-backup' mc upgrade
check_output 'refusal offers the override' '--no-backup'          mc upgrade

section 'Installing a plugin adds its commands'
install_plugins rcon
check_output 'mc plugins lists rcon'     'rcon'       mc plugins
check_output 'the rcon hooks are listed' 'pre-stop'   mc plugins
# It resolves now — the failure is a connection, not an unknown command.
check_false 'mc rcon no longer says unknown' \
    bash -c 'mc rcon 2>&1 | grep -q "Unknown command"'

install_plugins backup mrpack
check_output 'mc plugins lists backup'  'backup'  mc plugins
check_output 'mc plugins lists mrpack'  'mrpack'  mc plugins
check_output 'mrpack claims its extension' 'mrpack' mc plugins

section 'Only a registered name is dispatchable'
# Resolving to some executable is not sufficient. Without the registry, an
# internal entry point would be reachable from the command line, skipping the
# guards, the lock and the config loading its real entry point performs.
for internal in provide hook command serve-internal; do
    check_output "mc $internal is refused" 'Unknown command' mc "$internal"
done

section 'A plugin declaring an unknown ABI is refused by name'
cat > /usr/lib/mc/plugins.d/zz-future.toml <<'EOF'
abi  = 99
name = "future"
bin  = "/usr/libexec/mc/mc-rcon"
[[commands]]
name = "future"
EOF
check_output 'the offending plugin is named'  'future'  mc plugins
check_output 'the ABI mismatch is reported'   'ABI 99'  mc plugins
# And it must not take down anything else.
check_output 'healthy plugins still load'     'rcon'    mc plugins
check_true   'core still works'               mc --version
rm -f /usr/lib/mc/plugins.d/zz-future.toml

section 'A manifest whose binary is missing is refused, not deferred'
cat > /usr/lib/mc/plugins.d/zz-broken.toml <<'EOF'
abi  = 1
name = "broken"
bin  = "/usr/libexec/mc/mc-does-not-exist"
[[commands]]
name = "broken"
EOF
check_output 'the missing binary is reported' 'not an executable file' mc plugins
check_output 'its command is not dispatchable' 'Unknown command' mc broken
rm -f /usr/lib/mc/plugins.d/zz-broken.toml

section 'Removing a plugin withdraws its commands and leaves core working'
dpkg -r mc-rcon >/dev/null 2>&1
check_false 'the manifest is gone' test -f /usr/lib/mc/plugins.d/rcon.toml
check_false 'the binary is gone'   test -f /usr/libexec/mc/mc-rcon
check_output 'mc rcon is refused again' 'apt install mc-rcon' mc rcon
check_true   'core still works'    mc --version
check_true   'mc plugins still works' mc plugins
check_output 'the other plugins survive' 'backup' mc plugins

section 'A plugin cannot be installed without core'
# Depends: mc-server is what makes the plugin contract safe to assume.
dpkg -r mc-backup mc-mrpack >/dev/null 2>&1
dpkg -r mc-server >/dev/null 2>&1
check_false 'dpkg refuses a plugin with no core' \
    dpkg -i "$MC_DIST"/mc-rcon_*.deb

report
