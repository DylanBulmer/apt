#!/usr/bin/env bash
# mc-rcon plugin: adds the 'rcon' subcommand to mc

cmd_rcon() {
    require_server

    # is_running() asks systemd, which fails fast with a clear message under
    # a normal systemd install. Without systemd (e.g. this package running
    # inside a Docker image, where start.sh execs the server directly with no
    # systemd present), there's nothing for is_running() to ask — skip the
    # gate and let the rcon binary itself report a connection failure if the
    # server isn't actually up.
    if command -v systemctl >/dev/null 2>&1; then
        is_running || die "Server is not running."
    fi

    [[ -f "$PASSWD_FILE" ]] || die "RCON is not enabled. Install mc-rcon first, then run: mc install"

    load_config
    local port=$((SERVER_PORT + 10))

    # Pass the password by file so it never appears in argv (/proc/<pid>/cmdline)
    # or in a shell variable. With no extra args, rcon opens an interactive session.
    exec rcon --password-file "$PASSWD_FILE" 127.0.0.1 "$port" "$@"
}
