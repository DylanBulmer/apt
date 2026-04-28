#!/usr/bin/env bash
# ExecReload handler — sends 'reload' to the running server via RCON.
# Reloads datapacks and functions without a full restart.
set -euo pipefail

CONFIG_FILE="/etc/minecraft/server.conf"
PASSWD_FILE="/etc/minecraft/server.passwd"

if [[ ! -f "$PASSWD_FILE" ]]; then
    echo "[mc] RCON is not enabled. Install mc-rcon to enable systemctl reload." >&2
    exit 1
fi

if ! command -v rcon >/dev/null 2>&1; then
    echo "[mc] rcon not found. Install mc-rcon: apt install mc-rcon" >&2
    exit 1
fi

SERVER_PORT="25565"
[[ -f "$CONFIG_FILE" ]] && source "$CONFIG_FILE"
RCON_PORT=$((SERVER_PORT + 10))

password=$(cat "$PASSWD_FILE")
rcon 127.0.0.1 "$RCON_PORT" "$password" "reload"
