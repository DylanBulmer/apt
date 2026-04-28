#!/usr/bin/env bash
# ExecStop handler — sends in-game countdown warnings via RCON before systemd
# kills the server process. If RCON is unavailable the script exits immediately
# and systemd falls through to SIGTERM.
set -euo pipefail

CONFIG_FILE="/etc/minecraft/server.conf"
PASSWD_FILE="/etc/minecraft/server.passwd"

SERVER_PORT="25565"
[[ -f "$CONFIG_FILE" ]] && source "$CONFIG_FILE"
RCON_PORT=$((SERVER_PORT + 10))

rcon_say() {
    local password
    password=$(cat "$PASSWD_FILE")
    rcon 127.0.0.1 "$RCON_PORT" "$password" "say $*" 2>/dev/null || true
}

rcon_exec() {
    local password
    password=$(cat "$PASSWD_FILE")
    rcon 127.0.0.1 "$RCON_PORT" "$password" "$*" 2>/dev/null || true
}

# Only run the warning sequence if RCON is configured and reachable.
if [[ -f "$PASSWD_FILE" ]] && command -v rcon >/dev/null 2>&1; then
    rcon_say "[Server] Shutting down in 5 minutes."
    sleep 120
    rcon_say "[Server] Shutting down in 3 minutes."
    sleep 120
    rcon_say "[Server] Shutting down in 1 minute."
    sleep 60
    rcon_exec "stop"
    # Allow Minecraft time to flush chunks and exit cleanly before systemd
    # sends SIGTERM (TimeoutStopSec in the unit provides the outer bound).
    sleep 10
fi
