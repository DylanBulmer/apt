#!/usr/bin/env bash
# The source-provider path, with a real fixture pack.
#
# Tier 4 because it needs the plugin installed as a package and the provider
# invoked as a real subprocess. The manifest's own defences (traversal, host
# allowlist, missing hashes) are tier-1 cargo tests; what is left here is the
# protocol between core and the provider, and the invariant core keeps.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../../lib/assert.sh"

install_core
install_plugins rcon mrpack

# ── A fixture pack ──────────────────────────────────────────────────────────
# No `files` entries, so nothing is downloaded: the point is the protocol and
# the override merge, not the fetch, which the cargo tests cover.
build_pack() { # build_pack <output> <properties-body>
    local out="$1" props="$2" dir
    dir=$(mktemp -d)
    cat > "$dir/modrinth.index.json" <<'JSON'
{
  "formatVersion": 1,
  "name": "fixture",
  "versionId": "1.0.0",
  "dependencies": { "minecraft": "1.21.4" },
  "files": []
}
JSON
    mkdir -p "$dir/overrides/config"
    printf 'fixture=yes\n' > "$dir/overrides/config/pack.cfg"
    printf '%s' "$props" > "$dir/overrides/server.properties"
    ( cd "$dir" && zip -qr "$out" . )
    rm -rf "$dir"
}

section 'Installing from a pack'
# The pack asks for RCON with a password of its choosing, a port of its
# choosing, and a game port of its choosing. None of the three may survive.
build_pack /tmp/fixture.mrpack \
    'motd=From the pack
level-seed=12345
enable-rcon=true
rcon.password=attacker-chosen
rcon.port=31337
server-port=1234
'
check_true 'mc install accepts the pack' \
    mc install /tmp/fixture.mrpack --accept-eula --yes

check_true  'a server artifact landed'   test -s /opt/minecraft/server.jar
check_true  'the override tree merged'   test -f /opt/minecraft/config/pack.cfg
check_true  'the pack is recorded'       test -f /etc/minecraft/server.mrpack.json

section 'The pack got what it is allowed to set'
check_has 'motd came from the pack'      /opt/minecraft/server.properties 'motd=From the pack'
check_has 'level-seed came from the pack' /opt/minecraft/server.properties 'level-seed=12345'

section 'The pack did NOT get the keys the system owns'
# The whole reason core keeps the merge rather than letting the provider write
# server.properties: otherwise a pack enables RCON with a secret its author
# knows, and the server binds it.
check_lacks 'the pack password was rejected' /opt/minecraft/server.properties 'attacker-chosen'
check_lacks 'the pack rcon.port was rejected' /opt/minecraft/server.properties 'rcon.port=31337'
check_lacks 'the pack server-port was rejected' /opt/minecraft/server.properties 'server-port=1234'
# And the real password is the one on disk.
#
# The existence check is NOT redundant. Without it, a missing password file
# makes the substitution empty, `rcon.password=` matches the line the pack was
# stripped down to, and the assertion passes while proving nothing — which is
# exactly what it did the first time it was written.
check_true 'the password file exists' test -s /etc/minecraft/server.passwd
check 'rcon.password matches server.passwd' 'yes' \
    "$(grep -qF "rcon.password=$(cat /etc/minecraft/server.passwd)" \
        /opt/minecraft/server.properties && echo yes || echo no)"
check 'RCON was switched on by the hook' 'yes' \
    "$(grep -qx 'enable-rcon=true' /opt/minecraft/server.properties && echo yes || echo no)"

section 'Ownership survives the pack install'
check 'server.properties owner' 'minecraft:minecraft' "$(file_owner /opt/minecraft/server.properties)"
check 'server.properties mode'  '640'                 "$(file_mode  /opt/minecraft/server.properties)"
stray=$(find /opt/minecraft ! -user minecraft -print -quit)
check 'nothing in the tree is root-owned' '' "$stray"

section 'The config records what the pack pinned'
check_has 'version pinned'  /etc/minecraft/config.toml '1.21.4'
check_has 'type pinned'     /etc/minecraft/config.toml 'vanilla'

section 'A pack server refuses a bare version upgrade'
# It would replace the jar and strip every mod.
check_output 'refusal names .mrpack' '.mrpack' mc upgrade --version 1.21.3

section 'Removing the provider withdraws the capability'
dpkg -r mc-mrpack >/dev/null 2>&1
check_output 'a pack now names its package' 'apt install mc-mrpack' \
    mc install /tmp/fixture.mrpack --accept-eula --force

report
