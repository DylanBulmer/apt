#!/usr/bin/env bash
# One real `mc install` of $MC_TYPE against the live upstream APIs.
#
# Run by tests/run.sh --all, as four parallel containers (one per type) — each
# needs its own clean /opt/minecraft, and this is the only suite that touches
# the network. Writes /res/$MC_TYPE.txt for the runner to collect.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../lib/assert.sh"

t="${MC_TYPE:?set MC_TYPE to vanilla|paper|fabric|neoforge}"
mkdir -p /res

start=$(date +%s)
mc install --type "$t" --accept-eula --yes > "/res/$t.log" 2>&1
rc=$?
elapsed=$(( $(date +%s) - start ))

P=/opt/minecraft/server.properties
{
    section "$t (${elapsed}s)"
    check "install exits 0" 0 "$rc"

    # The whole point of --initSettings: a complete, editable properties file and
    # NO world, so level-seed can still be chosen.
    check "no world generated" "absent" "$([[ -d /opt/minecraft/world ]] && echo present || echo absent)"
    check "level-seed present" "yes" "$(grep -q '^level-seed=' "$P" 2>/dev/null && echo yes || echo no)"
    check "motd present"       "yes" "$(grep -q '^motd=' "$P" 2>/dev/null && echo yes || echo no)"

    # A four-key file means initialize_server_settings did not take effect.
    keys=$(grep -c = "$P" 2>/dev/null || echo 0)
    check "properties fully populated" "yes" "$([[ "${keys:-0}" -gt 40 ]] && echo yes || echo no)"

    check "mode 0640"  "640" "$(file_mode "$P" 2>/dev/null || echo missing)"
    check "owner"      "minecraft:minecraft" "$(stat -c '%U:%G' "$P" 2>/dev/null || echo missing)"
    check "eula recorded" "eula=true" "$(grep -h '^eula=' /opt/minecraft/eula.txt 2>/dev/null || echo missing)"

    # NeoForge's FML wrapper exits 1 even when --initSettings succeeded, so the
    # step judges the outcome instead. If this warning appears, that check
    # regressed — the file above is populated, so nothing actually failed.
    check_lacks "no spurious pre-generate warning" "/res/$t.log" "Could not pre-generate"

    printf '    version: %s\n' "$(grep -hE '^(SERVER_TYPE|MINECRAFT_VERSION|JAVA_VERSION)=' \
        /etc/minecraft/server.conf 2>/dev/null | tr '\n' ' ')"
    report
} > "/res/$t.txt" 2>&1
