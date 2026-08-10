#!/usr/bin/env bash
# Shared definitions for the mc toolchain.
#
# Sourced by:
#   /usr/lib/mc/lib.sh     — via /usr/bin/mc, as root
#   /usr/lib/mc/start.sh   — systemd ExecStart=,  as the minecraft user
#   /usr/lib/mc/stop.sh    — systemd ExecStop=,   as the minecraft user
#   /usr/lib/mc/reload.sh  — systemd ExecReload=, as the minecraft user
#
# CONSTRAINTS, because of that mixed set of callers:
#   * Definitions only. Sourcing this file must have no side effects: no writes,
#     no network, no systemctl. Half of the callers run unprivileged under
#     ProtectSystem=strict and must not fail merely by loading it.
#   * Must be safe under `set -euo pipefail` (every caller sets it).
#   * Must NOT depend on lib.sh's info()/warn()/error()/die(). Those colourise
#     for a terminal; the systemd-facing scripts log to the journal, where ANSI
#     escapes are noise. Functions here signal failure with a return code and
#     leave the reporting style to the caller.

# Idempotent: lib.sh and a plugin may both source this.
[[ -n "${_MC_COMMON_SOURCED:-}" ]] && return 0
_MC_COMMON_SOURCED="yes"

# ── Paths ──────────────────────────────────────────────────────────────────────

MC_BASE="/opt/minecraft"
MC_BACKUP="/var/backups/minecraft"
MC_CONFIG="/etc/minecraft"
DEFAULTS_CONF="$MC_CONFIG/defaults.conf"
SERVER_CONF="$MC_CONFIG/server.conf"
PASSWD_FILE="$MC_CONFIG/server.passwd"
MRPACK_MANIFEST="$MC_CONFIG/server.mrpack.json"
LOCK_FILE="/run/minecraft/mc.lock"
MC_USER="minecraft"

# Minecraft's stock port.
#
# Ports belong to the server, so they live in server.properties; mc's own config
# describes how to RUN the server, not what it is. This constant applies only
# where that file does not exist yet: seeding `server-port` into a new one, and
# standing in for the game port when computing the RCON port.
MC_STOCK_PORT=25565

# ── server.properties (read-only) ──────────────────────────────────────────────

# Read one key out of the live server.properties. Prints the value (empty when
# the key, or the file, is not there) and ALWAYS succeeds.
#
# This lives here rather than in lib.sh because server.properties is the source
# of truth for the keys the JVM owns — server-port, rcon.port, enable-rcon,
# rcon.password — and stop.sh/reload.sh have to resolve the RCON port through it
# without pulling in lib.sh. Everything that reads a property calls this, so
# there is one parser rather than several that can disagree.
#
# NEVER RETURNS NON-ZERO: every caller runs under `set -euo pipefail`, and a key
# that is simply absent is a normal answer, not an error. A `grep | cut` here
# would take grep's exit status, so an absent key would abort the whole mc
# invocation from a plain assignment. A missing or unreadable file is likewise
# empty — start.sh's own gate is what turns an unreadable file into a refusal,
# and it does so with a legible message.
mc_sprop_get() {
    local key="$1"
    local file="${2:-$MC_BASE/server.properties}"
    local line

    [[ -r "$file" ]] || return 0
    # '.' is a regex metacharacter and two of the managed keys contain one, so
    # an unescaped pattern would let rcon.port match "rconXport" as well.
    line=$(grep -m1 -- "^${key//./\\.}=" "$file" 2>/dev/null) || return 0
    printf '%s' "${line#*=}"
}

# ── Config ─────────────────────────────────────────────────────────────────────

# Populate mc's own settings — how to run the server: which build, which Java,
# how much heap, backup policy. In increasing order of precedence: built-in
# defaults, /etc/minecraft/defaults.conf, and /etc/minecraft/server.conf (the
# per-server file written by write_config).
#
# The server's OWN settings are not here and never were. Port, seed, MOTD,
# difficulty and RCON belong to server.properties, which the JVM reads and
# rewrites; read those with mc_sprop_get/mc_rcon_port at the point of use.
#
# ORDERING IS LOAD-BEARING. SERVER_TYPE is *derived* from DEFAULT_SERVER_TYPE,
# so it must be resolved AFTER defaults.conf is sourced. Resolving it earlier
# reads DEFAULT_SERVER_TYPE before the file that sets it has been loaded, and a
# `DEFAULT_SERVER_TYPE="paper"` there is silently ignored.
load_config() {
    MINECRAFT_VERSION="latest"
    JAVA_VERSION=""
    SERVER_RAM="4G"
    SERVER_FLAGS=""
    JAVA_OPTS=""
    BACKUP_KEEP="7"
    BACKUP_SCHEDULE="daily"

    # shellcheck source=/dev/null
    [[ -f "$DEFAULTS_CONF" ]] && source "$DEFAULTS_CONF"

    # Now that defaults.conf has had its say, derive the effective server type.
    # server.conf, sourced next, may override it with a concrete pinned value.
    SERVER_TYPE="${DEFAULT_SERVER_TYPE:-vanilla}"

    # shellcheck source=/dev/null
    [[ -f "$SERVER_CONF" ]] && source "$SERVER_CONF"
    return 0
}

# ── Java helpers ───────────────────────────────────────────────────────────────

# Map a Minecraft version to the Java major version it requires.
#
# SANITISE BEFORE COMPARING. `[[ "$major" -ge 26 ]]` is an *arithmetic* context:
# bash evaluates the operand as an expression, and while doing so it performs
# command substitution inside array subscripts. A value of
# 'PATH[$(rm -rf /)]' therefore executes the substitution — even under
# `set -euo pipefail`, because the base variable exists so the nounset check
# never fires. This function is handed the `minecraft` field of an untrusted
# .mrpack manifest by cmd_install_mrpack, which runs as root, so any component
# that is not a plain integer is forced to 0 rather than reaching the
# comparisons below.
mc_required_java() {
    local mc_ver="$1"
    local major minor patch
    IFS='.' read -r major minor patch <<< "$mc_ver"
    [[ "${major:-}" =~ ^[0-9]+$ ]] || major=0
    [[ "${minor:-}" =~ ^[0-9]+$ ]] || minor=0
    [[ "${patch:-}" =~ ^[0-9]+$ ]] || patch=0

    # Mojang switched to a new versioning scheme after 1.21.x.
    # Versions 26.x.x and above use the new format and require Java 25.
    if   [[ "$major" -ge 26 ]];                                then echo 25
    # Past 1.x.x versioning
    elif [[ "$minor" -ge 21 ]] \
      || [[ "$minor" -eq 20 && "$patch" -ge 5 ]];              then echo 21
    elif [[ "$minor" -ge 18 ]];                                then echo 17
    else                                                            echo 8
    fi
}

# Print the major version of a java binary (default: whatever is on PATH).
java_major_version() {
    local bin="${1:-java}"
    local raw
    raw=$("$bin" -version 2>&1 | awk -F '"' '/version/ { print $2 }')
    if [[ "$raw" == 1.* ]]; then
        echo "${raw#1.}" | cut -d. -f1
    else
        echo "${raw%%.*}"
    fi
}

# Locate a java binary for major version $1. Prints its path, or returns 1.
find_java_binary() {
    local required="$1"
    local bin

    while IFS= read -r bin; do
        [[ -x "$bin" ]] || continue
        [[ "$bin" =~ -${required}([^0-9]|$) ]] && { echo "$bin"; return 0; }
    done < <(update-alternatives --list java 2>/dev/null)

    local candidate
    for candidate in \
        "/usr/lib/jvm/java-${required}-openjdk-amd64/bin/java" \
        "/usr/lib/jvm/java-${required}-openjdk-arm64/bin/java" \
        "/usr/lib/jvm/java-${required}-openjdk/bin/java" \
        "/usr/lib/jvm/temurin-${required}-amd64/bin/java" \
        "/usr/lib/jvm/temurin-${required}/bin/java" \
        "/usr/lib/jvm/java-${required}-amazon-corretto-amd64/bin/java" \
        "/usr/lib/jvm/java-${required}-amazon-corretto/bin/java"; do
        [[ -x "$candidate" ]] && { echo "$candidate"; return 0; }
    done

    return 1
}

# ── EULA ───────────────────────────────────────────────────────────────────────

# True when eula.txt records acceptance of the Minecraft EULA.
#
# Shared with start.sh, which gates the launch on it — hence its home here
# rather than in lib.sh. Mojang's own file is a comment header followed by
# `eula=true`, and operators edit it by hand, so tolerate surrounding
# whitespace and TRUE/True. Anything else (absent file, eula=false, a value
# commented out) is a refusal: this decides whether a licence was accepted, so
# it fails closed.
eula_accepted() {
    local file="${1:-$MC_BASE/eula.txt}"
    [[ -f "$file" ]] || return 1
    grep -qiE '^[[:space:]]*eula[[:space:]]*=[[:space:]]*true[[:space:]]*$' "$file"
}

# ── RCON ───────────────────────────────────────────────────────────────────────

# The port to dial for RCON.
#
# rcon.port in server.properties is checked FIRST and used verbatim, because it
# is the port the JVM binds. The +10 convention below is only this toolchain's
# default for a server it is setting up; an operator who sets rcon.port by hand
# — or a modpack that ships one — is not obliged to follow it, and deriving the
# port unconditionally meant every such server had a working RCON listener that
# nothing in mc could reach. That failure is quiet and expensive: stop.sh reads
# an unreachable RCON as "player count unknown" and takes the 5-minute countdown
# on every stop, and cmd_backup loses save-off/save-all and archives a world that
# was never flushed.
#
# Order: what the JVM binds → the convention applied to the live game port → the
# convention applied to the stock port. The last tier only applies before a
# server.properties exists at all; it keeps this total rather than failing.
mc_rcon_port() {
    local port

    port=$(mc_sprop_get "rcon.port")
    if [[ "$port" =~ ^[0-9]+$ ]]; then
        printf '%s\n' "$port"
        return 0
    fi

    port=$(mc_sprop_get "server-port")
    # Same reasoning as mc_required_java: never let an unvalidated string reach
    # an arithmetic context. Anything that is not a plain integer falls back
    # rather than being evaluated as an expression.
    [[ "$port" =~ ^[0-9]+$ ]] || port="$MC_STOCK_PORT"
    echo $(( port + 10 ))
}

# Generate an RCON password: 24 random bytes in base64url.
#
# THE CHARSET IS LOAD-BEARING, not cosmetic. base64url (A-Za-z0-9-_) excludes
# every character that would need escaping downstream — in particular '|', which
# mc-rcon's postinst uses as its sed delimiter when writing rcon.password into
# server.properties, and '=', which would be ambiguous in a properties line. The
# `tr` calls are what enforce that: '+/' are folded to '-_' and '=' padding is
# stripped.
#
# Defined here rather than in lib.sh so the maintainer scripts, which source
# only what they need, share this one definition of the charset.
generate_rcon_password() {
    head -c 24 /dev/urandom | base64 | tr '+/' '-_' | tr -d '='
}

# True when RCON is usable: a password file exists and the client is installed
# (the mc-rcon package provides it; RCON is off by default without it).
mc_rcon_available() {
    [[ -f "$PASSWD_FILE" ]] && command -v rcon >/dev/null 2>&1
}

# Build the command that broadcasts $* to every player with no sender prefix.
#
# NOT `say`. The server renders `say <msg>` as "[<sender>] <msg>", and for a
# command arriving over RCON that sender is literally "Rcon", so players see
# "[Rcon] [Server] …". tellraw writes a raw chat component with no attribution.
#
# Trade-off worth knowing: `say` is echoed to the server console, and so into
# the journal, while tellraw is not. Both callers write their own line to
# stderr, which is where the journal copy comes from.
#
# THE TEXT IS ESCAPED, NOT TRUSTED. Every message today is an internal literal,
# but this string is interpolated into a JSON document that the server parses
# and acts on: a bare '"' would close the component early and leave the rest to
# be read as further JSON.
mc_say_command() {
    local msg="$*"
    msg="${msg//\\/\\\\}"      # backslashes first, or the escaping below doubles up
    msg="${msg//\"/\\\"}"
    msg="${msg//$'\n'/ }"      # a component is a single line; a raw newline is invalid JSON
    msg="${msg//$'\r'/ }"
    printf 'tellraw @a {"text":"%s"}' "$msg"
}

# Default wall-clock budget for a single RCON call, in seconds. stop.sh overrides
# this per call kind (a fire-and-forget chat "say" gets a shorter leash than a
# "list"/"stop" whose result is acted on); everything else uses this.
MC_RCON_TIMEOUT=15

# Run one RCON command against the local server.
#   $1  wall-clock budget in seconds; 0 disables the timeout wrapper
#   $@  the command words
#
# Returns non-zero if RCON is unavailable, the budget is exceeded, or the call
# itself fails. stderr from rcon(1) is passed through — callers that want it
# quiet redirect at the call site, so that reload.sh can surface real errors.
#
# The password is always passed by file, never in argv (/proc/<pid>/cmdline is
# world-readable) and never through the environment.
#
# rcon(1) enforces its own per-exchange deadline (CMD_DEADLINE_S), but a single
# invocation performs an auth exchange *and* a command exchange, so its internal
# worst case is roughly double that. `timeout` is the outer authoritative bound;
# the internal deadline is what keeps the no-`timeout` fallback survivable.
mc_rcon_call() {
    local budget="${1:-0}"; shift
    mc_rcon_available || return 1

    local port
    port=$(mc_rcon_port)

    if [[ "$budget" -gt 0 ]] && command -v timeout >/dev/null 2>&1; then
        timeout "$budget" rcon --password-file "$PASSWD_FILE" 127.0.0.1 "$port" "$@"
    else
        rcon --password-file "$PASSWD_FILE" 127.0.0.1 "$port" "$@"
    fi
}
