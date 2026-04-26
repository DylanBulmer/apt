#!/usr/bin/env bash
# mc-rcon plugin: adds the 'rcon' subcommand to mc

cmd_rcon() {
    require_name "${1:-}"
    local name="$1"
    shift || true
    require_server "$name"
    is_running "$name" || die "Server '$name' is not running."

    local passwd_file="$MC_CONFIG/${name}.passwd"
    [[ -f "$passwd_file" ]] || die "No RCON password file: $passwd_file"

    load_config "$name"
    local port=$((SERVER_PORT + 10))
    local password
    password=$(cat "$passwd_file")

    # With no extra args, mcrcon opens an interactive session
    exec mcrcon 127.0.0.1 "$port" "$password" "$@"
}
