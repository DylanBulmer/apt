#!/usr/bin/env bash
# mc-rcon plugin: adds the 'rcon' subcommand to mc

cmd_rcon() {
    require_server
    is_running || die "Server is not running."

    [[ -f "$PASSWD_FILE" ]] || die "RCON is not enabled. Install mc-rcon first, then run: mc install"

    load_config
    local port=$((SERVER_PORT + 10))
    local password
    password=$(cat "$PASSWD_FILE")

    # With no extra args, mcrcon opens an interactive session.
    exec mcrcon 127.0.0.1 "$port" "$password" "$@"
}
