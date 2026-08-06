#!/usr/bin/env bash
# ExecReload handler — sends 'reload' to the running server via RCON.
# Reloads datapacks and functions without a full restart.
set -euo pipefail

# shellcheck source=/usr/lib/mc/common.sh
source /usr/lib/mc/common.sh

if [[ ! -f "$PASSWD_FILE" ]]; then
    echo "[mc] RCON is not enabled. Install mc-rcon to enable systemctl reload." >&2
    exit 1
fi

if ! command -v rcon >/dev/null 2>&1; then
    echo "[mc] rcon not found. Install mc-rcon: apt install mc-rcon" >&2
    exit 1
fi

load_config

# Unlike stop.sh, this one does NOT swallow stderr: `systemctl reload` is an
# interactive, operator-initiated action, so a connection or auth failure should
# be reported rather than silently returning non-zero.
mc_rcon_call "$MC_RCON_TIMEOUT" reload
