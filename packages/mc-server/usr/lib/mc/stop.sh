#!/usr/bin/env bash
# ExecStop handler — sends in-game countdown warnings via RCON before systemd
# kills the server process. If RCON is unavailable the script exits immediately
# and systemd falls through to SIGTERM.
#
# The countdown is skipped entirely when nobody is online, so that
# `mc restart` / `mc upgrade` on an idle server are near-instant instead of
# always burning 5 minutes. If even one player is connected — or if the player
# count cannot be determined for ANY reason — the full 5-minute countdown runs.
# An unnecessary wait on an empty server is far cheaper than cutting off real
# players without warning, so every uncertain case resolves to the long path.
set -euo pipefail

# shellcheck source=/usr/lib/mc/common.sh
source /usr/lib/mc/common.sh
load_config

# ── Bounded RCON invocation ────────────────────────────────────────────────
# EVERY rcon call from this script must be bounded in wall-clock terms, because
# this script is systemd's ExecStop= and overrunning TimeoutStopSec means the
# JVM is SIGKILLed mid-chunk-flush — the world corruption the countdown exists
# to prevent.
#
# rcon(1) applies its own 30 s deadline per exchange (CMD_DEADLINE_S), but one
# invocation performs an auth exchange *and* a command exchange, so its own
# worst case is ~60 s. `timeout` is the outer, authoritative bound; rcon's
# internal deadline is what keeps the no-`timeout` fallback below survivable.
#
# Two budgets, because the calls differ in kind:
#   SAY  — fire-and-forget chat. Nothing depends on the reply, so a short
#          leash is fine and keeps the countdown close to its advertised times.
#   CMD  — `list` and `stop`, whose result (or effect) we actually act on.
RCON_SAY_TIMEOUT=5
RCON_CMD_TIMEOUT=15

# mc_rcon_call (common.sh) applies the budget and passes the password by file.
# Its stderr is passed through by default; this script wants it quiet.
rcon_call() {
    local budget="$1"; shift
    mc_rcon_call "$budget" "$@" 2>/dev/null
}

# mc_say_command (common.sh) builds a tellraw rather than a `say`, so the
# countdown does not reach players prefixed with "[Rcon]".
rcon_say() {
    rcon_call "$RCON_SAY_TIMEOUT" "$(mc_say_command "$*")" || true
}

rcon_exec() {
    rcon_call "$RCON_CMD_TIMEOUT" "$*" || true
}

# ── Player count ───────────────────────────────────────────────────────────
# Ask the server how many players are online and print that number on stdout.
# Returns non-zero (printing nothing) for every failure mode — rcon missing,
# connection refused, auth failure, timeout, empty output, or wording we don't
# recognise — so that callers fall back to the conservative full countdown.
player_count() {
    local raw norm n

    # rcon_call bounds a wedged connection (see RCON_CMD_TIMEOUT above). A
    # timeout exits non-zero, which is exactly the "unknown" signal we want.
    raw=$(rcon_call "$RCON_CMD_TIMEOUT" list) || return 1

    [[ -n "$raw" ]] || return 1

    # Normalise: strip Minecraft '§x' colour codes (some forks colourise the
    # reply), flatten newlines/tabs to spaces and fold to lower case. LC_ALL=C
    # keeps this byte-deterministic regardless of the caller's locale.
    norm=$(printf '%s' "$raw" \
             | LC_ALL=C sed 's/§.//g' \
             | LC_ALL=C tr 'A-Z\n\r\t' 'a-z   ') || return 1

    # Known reply shapes, most specific first:
    #   vanilla/paper  "There are 3 of a max of 20 players online: a, b, c"
    #   spigot/bukkit  "There are 3 out of maximum 20 players online."
    #   some forks     "There are 3/20 players online"
    #   some mods      "3/20 players online"  /  "3 players online"
    # Anything that matches none of these is treated as UNKNOWN, never as 0.
    n=""
    if [[ "$norm" =~ (^|[^0-9a-z])are[[:space:]]+([0-9]+) ]]; then
        n="${BASH_REMATCH[2]}"
    elif [[ "$norm" =~ ([0-9]+)[[:space:]]*/[[:space:]]*[0-9]+ ]]; then
        n="${BASH_REMATCH[1]}"
    elif [[ "$norm" =~ ([0-9]+)[[:space:]]+(of|out)[[:space:]] ]]; then
        n="${BASH_REMATCH[1]}"
    elif [[ "$norm" =~ ([0-9]+)[[:space:]]+players?[[:space:]]+online ]]; then
        n="${BASH_REMATCH[1]}"
    else
        return 1
    fi

    # Belt and braces: the captures above can only be digits, but never let a
    # non-numeric or absurd value reach the arithmetic comparison below.
    [[ "$n" =~ ^[0-9]+$ ]] || return 1
    [[ ${#n} -le 6 ]] || return 1

    # 10# guards against a zero-padded count ("08") being read as octal.
    printf '%s' "$((10#$n))"
}

# ── Countdown ──────────────────────────────────────────────────────────────
# Announce a shutdown at each given "seconds remaining" mark, sleeping through
# the gaps, and return once the final mark has elapsed.
#   countdown 300 180 60  →  warn at 5 min, sleep 120, warn at 3 min,
#                            sleep 120, warn at 1 min, sleep 60.
#
# KEPT GENERAL ON PURPOSE. The only surviving call site is `countdown 300 180
# 60`, whose marks are all whole minutes, so the sub-minute "in N seconds"
# branch below is currently unreachable. It is retained rather than deleted
# because the tier policy above is a knob the operator is expected to turn, and
# a general helper makes re-tiering a one-line change instead of a rewrite.
# The alternative — inlining three rcon_say calls and three sleeps — would be
# both longer and harder to keep consistent. Do not "clean up" the seconds
# branch without also fixing the tier table.
countdown() {
    local prev=0 mark mins
    for mark in "$@"; do
        if [[ $prev -gt 0 ]]; then
            sleep $((prev - mark))
        fi
        if [[ $mark -ge 60 && $((mark % 60)) -eq 0 ]]; then
            mins=$((mark / 60))
            if [[ $mins -eq 1 ]]; then
                rcon_say "[Server] Shutting down in 1 minute."
            else
                rcon_say "[Server] Shutting down in $mins minutes."
            fi
        else
            rcon_say "[Server] Shutting down in $mark seconds."
        fi
        prev=$mark
    done
    if [[ $prev -gt 0 ]]; then
        sleep "$prev"
    fi
}

# Only run the warning sequence if RCON is configured and reachable.
if mc_rcon_available; then
    # Empty string means "could not determine" — see player_count().
    PLAYERS=$(player_count) || PLAYERS=""

    # ── Warning tiers ──────────────────────────────────────────────────────
    # Two outcomes only. Either the server is provably empty and we stop at
    # once, or somebody might be affected and they get the full warning:
    #
    #   players   warning   announced at
    #   ---------------------------------------------------------------
    #   0         none      (stop immediately)
    #   1+        5 min     5 min, 3 min, 1 min
    #   unknown   5 min     5 min, 3 min, 1 min   (fail safe)
    #
    # The last two rows behave identically but are logged differently on
    # purpose: the journal must distinguish "we counted N players" from "we
    # could not count at all", because the latter also indicates RCON trouble.
    if [[ -z "$PLAYERS" ]]; then
        echo "[mc] Player count unavailable — using full 5-minute countdown." >&2
        countdown 300 180 60
    elif [[ "$PLAYERS" -eq 0 ]]; then
        echo "[mc] No players online — stopping immediately." >&2
    else
        echo "[mc] $PLAYERS player(s) online — 5-minute countdown." >&2
        countdown 300 180 60
    fi

    rcon_exec "stop"
    # Allow Minecraft time to flush chunks and exit cleanly before systemd
    # sends SIGTERM (TimeoutStopSec in the unit provides the outer bound).
    sleep 10
fi
