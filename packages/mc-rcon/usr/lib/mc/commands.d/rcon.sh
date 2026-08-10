#!/usr/bin/env bash
# mc-rcon plugin: adds the 'rcon' subcommand to mc

# Declare the subcommand so /usr/bin/mc will dispatch it. Without this the
# dispatcher rejects 'rcon' even though cmd_rcon is defined.
mc_register_command rcon

cmd_rcon() {
    # ── State verbs ────────────────────────────────────────────────────────
    # enable/disable/status act on server.properties rather than talking to a
    # running server, so they are handled before the connection is set up — you
    # must be able to enable RCON on a server that is stopped, or on one where
    # RCON is precisely what is currently off.
    #
    # They shadow any server command of the same name. None exist today, and
    # `mc rcon -- <command>` sends one literally if that ever changes.
    case "${1:-}" in
        enable|disable|status)
            local verb="$1"; shift
            [[ $# -eq 0 ]] || die "mc rcon ${verb} takes no arguments."
            # enable/disable rewrite server.properties and may provision the
            # password file, so they need root: the file is 0640 owned by the
            # service account, which makes it readable by the minecraft group
            # but writable only by its owner, and /etc/minecraft is root-owned.
            # status only reads those files, which the group may already do.
            if [[ "$verb" == "status" ]]; then
                require_root_or_group
            else
                require_root
            fi
            require_server
            load_config
            cmd_rcon_"$verb"
            return
            ;;
        --) shift ;;
    esac

    # Access check first, and before require_server: everything this path
    # touches is closed to a user outside the minecraft group — MC_BASE is 0750
    # minecraft:minecraft, the password file is 0640 root:minecraft, and
    # server.properties (which carries the port) is 0640 minecraft:minecraft.
    # Checking server_installed first would fail its -f test purely because the
    # directory is untraversable and report "No server installed" to a user
    # whose server is installed and running.
    #
    # A session only reads those files, so group membership is enough; see
    # require_root_or_group for why root would buy nothing here.
    require_root_or_group
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

    [[ -f "$PASSWD_FILE" ]] || die "RCON is not enabled. Run: mc rcon enable"

    load_config
    local port
    port=$(mc_rcon_port)

    # Deliberately NOT routed through mc_rcon_call: this exec's into rcon so the
    # user gets a real interactive session, and it must not carry a timeout — an
    # operator sitting at the `rcon>` prompt would otherwise be cut off mid-session.
    #
    # Pass the password by file so it never appears in argv (/proc/<pid>/cmdline)
    # or in a shell variable. With no extra args, rcon opens an interactive session.
    exec rcon --password-file "$PASSWD_FILE" 127.0.0.1 "$port" "$@"
}

# Turn RCON on. set_rcon_enabled() (mc-server) provisions the password if it is
# missing and writes the three managed keys; it reports whether anything moved.
cmd_rcon_enable() {
    if set_rcon_enabled true; then
        info "RCON enabled on port $(mc_rcon_port)."
        # NOT restarted automatically. The server reads server.properties at
        # startup, so a restart is required — but on a populated server that is
        # a five-minute countdown, and choosing when to spend it is the
        # operator's call, not a side effect of a config command.
        if is_running; then
            info "Restart to apply: mc restart"
        fi
    else
        info "RCON is already enabled on port $(mc_rcon_port)."
    fi
}

# Turn RCON off, clearing the password out of server.properties. The password
# file itself is left alone, so `mc rcon enable` restores the same secret rather
# than inventing a new one every time it is toggled.
cmd_rcon_disable() {
    if set_rcon_enabled false; then
        info "RCON disabled."
        if is_running; then
            info "Restart to apply: mc restart"
        fi
    else
        info "RCON is already disabled."
    fi
}

cmd_rcon_status() {
    local enabled port
    enabled=$(mc_sprop_get enable-rcon)
    port=$(mc_rcon_port)

    info "enable-rcon: ${enabled:-unset} (port ${port})"

    if [[ ! -f "$PASSWD_FILE" ]]; then
        warn "No password file at ${PASSWD_FILE} — run: mc rcon enable"
    elif [[ "$(mc_sprop_get rcon.password)" != "$(cat "$PASSWD_FILE")" ]]; then
        # The two drift apart if server.properties was edited by hand or
        # restored from a backup taken before the password was provisioned.
        warn "server.properties disagrees with ${PASSWD_FILE} — run: mc rcon enable"
    fi

    if [[ "$enabled" == "true" ]] && is_running; then
        # Proves the whole path end to end: password, port, and a listening
        # server — rather than just what the file claims.
        if mc_rcon_call 5 list >/dev/null 2>&1; then
            info "Connection: OK"
        else
            warn "Connection: FAILED — the server may need a restart to pick up the settings."
        fi
    fi
}
