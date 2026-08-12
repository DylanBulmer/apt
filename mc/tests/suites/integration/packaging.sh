#!/usr/bin/env bash
# What the packages actually put on disk, as dpkg installs them.
#
# Tier 4: everything here needs a real dpkg and a real root. The *content* of
# these files is covered by cargo tests; this is about metadata, modes and
# ownership, which only exist once installed.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../../lib/assert.sh"

install_all

section 'Control metadata'
for pkg in mc-server mc-rcon mc-mgmt mc-backup mc-mrpack; do
    check "$pkg is installed" 'install ok installed' \
        "$(dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null | sed 's/^[^ ]* [^ ]* //;s/^/install ok /')"
    # The architecture must be a real one. `any` is a source-level wildcard and
    # a binary package carrying it is one apt will refuse to install.
    arch=$(dpkg-query -W -f='${Architecture}' "$pkg" 2>/dev/null)
    check "$pkg architecture is concrete" 'yes' \
        "$([[ "$arch" != "any" && "$arch" != "all" && -n "$arch" ]] && echo yes || echo no)"
done

section 'Dependency substitution'
# ${shlibs:Depends} is a debhelper variable and nothing substitutes it under
# plain dpkg-deb. Shipping it literally produces a package apt refuses.
for pkg in mc-server mc-rcon mc-mgmt mc-backup mc-mrpack; do
    deps=$(dpkg-query -W -f='${Depends}' "$pkg" 2>/dev/null)
    check "$pkg has no unsubstituted variable" 'no' \
        "$([[ "$deps" == *'${'* ]] && echo yes || echo no)"
    check "$pkg depends on a libc" 'yes' \
        "$([[ "$deps" == *libc6* ]] && echo yes || echo no)"
done

section 'Recommends make a bare install batteries-included'
recommends=$(dpkg-query -W -f='${Recommends}' mc-server 2>/dev/null)
for plugin in mc-rcon mc-mgmt mc-backup mc-mrpack; do
    check "mc-server recommends $plugin" 'yes' \
        "$([[ "$recommends" == *"$plugin"* ]] && echo yes || echo no)"
done

section 'Installed layout'
check_true 'the dispatcher is installed'   test -x /usr/bin/mc
check_true 'the standalone client is installed' test -x /usr/bin/rcon
for plugin in mc-rcon mc-mgmt mc-backup mc-mrpack; do
    check_true "$plugin binary is executable" test -x "/usr/libexec/mc/$plugin"
done
# The shell implementation is gone. A leftover .sh would be dead code that an
# operator could still source, and a maintainer could still edit believing it
# ran.
check "no shell library survives" '0' \
    "$(find /usr/lib/mc -name '*.sh' 2>/dev/null | wc -l | tr -d ' ')"

section 'Every plugin manifest points at a real executable'
# Catches the packaging slip the ABI number cannot: a manifest shipped without
# its binary, or a path that drifted between the manifest and build.sh.
for manifest in /usr/lib/mc/plugins.d/*.toml; do
    bin=$(grep -E '^bin[[:space:]]*=' "$manifest" | head -1 | sed 's/.*= *"\(.*\)".*/\1/')
    check_true "$(basename "$manifest") -> $bin exists" test -x "$bin"
    check "$(basename "$manifest") declares abi 1" '1' \
        "$(grep -E '^abi[[:space:]]*=' "$manifest" | head -1 | sed 's/[^0-9]//g')"
done

section 'Modes and ownership'
check 'server dir mode'         '750'                "$(file_mode  /opt/minecraft)"
check 'server dir owner'        'minecraft:minecraft' "$(file_owner /opt/minecraft)"
# root:root 0700 on purpose: the service account runs untrusted mods, and owning
# this directory would let it pre-create the next predictable archive name as a
# symlink for root's writer to follow.
check 'backup dir mode'         '700'                "$(file_mode  /var/backups/minecraft)"
check 'backup dir owner'        'root:root'          "$(file_owner /var/backups/minecraft)"
check 'config dir mode'         '755'                "$(file_mode  /etc/minecraft)"
check 'config dir owner'        'root:root'          "$(file_owner /etc/minecraft)"

section 'Program files are not writable by anyone but root'
for f in /usr/bin/mc /usr/bin/rcon /usr/libexec/mc/mc-rcon \
         /usr/libexec/mc/mc-mgmt /usr/libexec/mc/mc-backup /usr/libexec/mc/mc-mrpack \
         /usr/lib/mc/plugins.d/rcon.toml /usr/lib/mc/plugins.d/mgmt.toml \
         /etc/minecraft/config.toml; do
    mode=$(file_mode "$f")
    check "$f group/other not writable" 'yes' \
        "$([[ $(( 8#$mode & 8#022 )) -eq 0 ]] && echo yes || echo no)"
    check "$f owned by root" 'root:root' "$(file_owner "$f")"
done

section 'The service account exists and cannot log in'
check 'minecraft user exists' 'yes' "$(id minecraft >/dev/null 2>&1 && echo yes || echo no)"
check 'shell is nologin' 'yes' \
    "$(getent passwd minecraft | grep -qE '(nologin|false)$' && echo yes || echo no)"

section 'Conffiles'
# config.toml must be a conffile, or an upgrade silently discards the
# operator's settings.
check_has 'config.toml is a conffile' /var/lib/dpkg/status '/etc/minecraft/config.toml'

section 'Units'
check_true 'minecraft.service installed'        test -f /lib/systemd/system/minecraft.service
check_true 'backup timer installed'             test -f /lib/systemd/system/minecraft-backup.timer
# The exec targets must be the Rust dispatcher, and must be exactly the three
# subcommands the privilege table declares unprivileged.
check_has 'ExecStart is mc serve'    /lib/systemd/system/minecraft.service 'ExecStart=/usr/bin/mc serve'
check_has 'ExecStop is mc shutdown'  /lib/systemd/system/minecraft.service 'ExecStop=/usr/bin/mc shutdown'
check_has 'ExecReload is mc reload'  /lib/systemd/system/minecraft.service 'ExecReload=/usr/bin/mc reload'
check_has 'exit 78 prevents restart' /lib/systemd/system/minecraft.service 'RestartPreventExitStatus=78'
# Asserted here as well as in the cargo test, because the value and the
# arithmetic it comes from live in different repositories of truth.
check_has 'stop timeout matches the countdown' /lib/systemd/system/minecraft.service 'TimeoutStopSec=380s'
check_has 'the unit runs unprivileged' /lib/systemd/system/minecraft.service 'User=minecraft'
# Grepped as a DIRECTIVE, not as a string: the comment above it in the unit
# explains at length why there is no SuccessExitStatus=, and a naive search
# matches the explanation.
check 'no SuccessExitStatus directive' '0' \
    "$(grep -cE '^[[:space:]]*SuccessExitStatus=' /lib/systemd/system/minecraft.service)"

section 'Manual pages'
# The Debian image excludes /usr/share/man from dpkg's unpack by default, so
# these assert what the .deb CONTAINS rather than what landed on disk — which
# is the honest question anyway: a page missing from the archive is missing on
# every normal machine too.
#
# Each listing is captured ONCE into a variable and matched with a here-string.
# `dpkg-deb -c … | grep -q` looks equivalent and is not: grep exits at the
# first hit, dpkg-deb dies of SIGPIPE, and `set -o pipefail` turns that into a
# failed check for every page that is not near the end of the archive.
listing() { dpkg-deb -c "$(ls "$MC_DIST/$1"_*.deb 2>/dev/null | head -1)" 2>/dev/null; }

for pkg in mc-server mc-rcon mc-mgmt mc-backup mc-mrpack; do
    contents=$(listing "$pkg")
    case "$pkg" in
        mc-server) pages=(man1/mc.1.gz man5/mc-config.5.gz man5/mc-plugins.5.gz) ;;
        mc-rcon)   pages=(man1/mc-rcon.1.gz man1/rcon.1.gz) ;;
        mc-mgmt)   pages=(man1/mc-mgmt.1.gz) ;;
        mc-backup) pages=(man1/mc-backup.1.gz man1/mc-restore.1.gz) ;;
        mc-mrpack) pages=(man1/mc-mrpack.1.gz) ;;
    esac
    for page in "${pages[@]}"; do
        check "$pkg ships ${page##*/}" 'yes' \
            "$(grep -qF " ./usr/share/man/$page" <<<"$contents" && echo yes || echo no)"
    done
done

# The .so redirect must be a symlink to the COMPRESSED target: gzipping the
# stub instead would leave `man mc-restore` chasing a name that no longer
# exists.
check 'mc-restore.1.gz redirects to mc-backup.1.gz' 'yes' \
    "$(grep -qE 'mc-restore\.1\.gz -> .*mc-backup\.1\.gz' <<<"$(listing mc-backup)" \
        && echo yes || echo no)"

# The generated page is the one built from THIS source tree, not a stale copy.
mc_page=$(dpkg-deb --fsys-tarfile "$(ls "$MC_DIST"/mc-server_*.deb | head -1)" 2>/dev/null \
    | tar -xO ./usr/share/man/man1/mc.1.gz 2>/dev/null | gzip -dc)
check 'mc.1 documents the man subcommand' 'yes' \
    "$(grep -qF 'mc man' <<<"$mc_page" && echo yes || echo no)"
check 'mc.1 documents the install flags' 'yes' \
    "$(grep -qF 'accept\-eula' <<<"$mc_page" && echo yes || echo no)"

section 'The postinst reloads systemd rather than gating on health'
# `systemctl is-system-running` is a HEALTH check, non-zero for "degraded" —
# any machine with one unrelated failed unit. The stub reports degraded, so if
# the postinst gated on it this reload would be missing.
check_has 'daemon-reload was called' /tmp/systemctl.log 'daemon-reload'

report
