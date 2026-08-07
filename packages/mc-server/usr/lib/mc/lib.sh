#!/usr/bin/env bash
# Core library sourced by /usr/bin/mc

# Paths, config loading, Java resolution and RCON invocation are shared with the
# systemd-facing scripts (start/stop/reload), which cannot source this file —
# they run unprivileged and need none of the command implementations below.
# shellcheck source=/usr/lib/mc/common.sh
source /usr/lib/mc/common.sh

# ── Output helpers ─────────────────────────────────────────────────────────────

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'

info()  { echo -e "${GREEN}[mc]${NC} $*"; }
warn()  { echo -e "${YELLOW}[mc]${NC} $*" >&2; }
error() { echo -e "${RED}[mc]${NC} $*" >&2; }
die()   { error "$*"; exit 1; }

require_root() {
    [[ $EUID -eq 0 ]] || die "This command must be run as root."
}

# True when a server is present. NeoForge installs a run.sh instead of a
# plain server.jar, so both count.
server_installed() {
    [[ -f "$MC_BASE/server.jar" || -f "$MC_BASE/run.sh" ]]
}

require_server() {
    server_installed || die "No server installed. Run: mc install"
}

# ── Plugin command registry ────────────────────────────────────────────────────

# Subcommands contributed by plugins in /usr/lib/mc/commands.d/ (e.g. mc-rcon
# adds 'rcon'). A plugin declares itself with:
#
#     mc_register_command rcon
#
# /usr/bin/mc dispatches an unrecognised subcommand ONLY if it appears here.
# Previously the dispatcher accepted any name resolving to a `cmd_*` function,
# which made internal helpers reachable from the command line — `mc
# install_mrpack pack.mrpack` entered cmd_install_mrpack directly, bypassing the
# require_root, acquire_lock and load_config that cmd_install performs first.
MC_PLUGIN_COMMANDS=()

mc_register_command() {
    local name
    for name in "$@"; do
        [[ -n "$name" ]] && MC_PLUGIN_COMMANDS+=("$name")
    done
    return 0
}

# True if $1 was registered by a plugin.
mc_is_plugin_command() {
    local target="${1:-}" name
    [[ -n "$target" ]] || return 1
    for name in ${MC_PLUGIN_COMMANDS[@]+"${MC_PLUGIN_COMMANDS[@]}"}; do
        [[ "$name" == "$target" ]] && return 0
    done
    return 1
}

# ── Config ──────────────────────────────────────────────────────────────

# load_config() is in common.sh — start.sh needs it too.

# Persist the effective configuration to $SERVER_CONF.
#
# EVERY VALUE IS WRITTEN THROUGH %q. load_config() *sources* this file as root,
# so an unquoted value is a code-execution sink: a MINECRAFT_VERSION containing
# a newline used to append its own line to the file, and one containing $(...)
# had it run on the next `mc` invocation. Values reaching here are validated at
# their entry points, but this file is the last line of defence and the one that
# turns a one-shot parsing slip into persistent root execution — %q makes the
# output shell-safe regardless of what the value contains.

write_config() {
    # BACKUP_SCHEDULE is interpolated into a systemd unit drop-in below, where
    # %q is no help — that file is unit syntax, not shell. Require a single line
    # so it cannot append arbitrary directives to a unit that runs as root.
    # Checked up front so a bad value aborts before anything is written.
    if [[ "$BACKUP_SCHEDULE" == *$'\n'* || "$BACKUP_SCHEDULE" == *$'\r'* ]]; then
        die "Invalid BACKUP_SCHEDULE: must be a single line of systemd OnCalendar= syntax."
    fi

    mkdir -p "$MC_CONFIG"
    {
        echo "# mc server configuration"
        printf 'SERVER_TYPE=%q\n'       "$SERVER_TYPE"
        printf 'MINECRAFT_VERSION=%q\n' "$MINECRAFT_VERSION"
        printf 'JAVA_VERSION=%q\n'      "$JAVA_VERSION"
        printf 'SERVER_RAM=%q\n'        "$SERVER_RAM"
        printf 'SERVER_PORT=%q\n'       "$SERVER_PORT"
        printf 'BACKUP_KEEP=%q\n'       "$BACKUP_KEEP"
        printf 'BACKUP_SCHEDULE=%q\n'   "$BACKUP_SCHEDULE"
        printf 'JAVA_OPTS=%q\n'         "$JAVA_OPTS"
    } > "$SERVER_CONF"

    # Regenerate the backup timer drop-in, and reload systemd if it moved.
    #
    # THE RELOAD BELONGS HERE, next to the write that makes it necessary. It
    # used to be reported to the caller through a _MC_TIMER_DROPIN_CHANGED
    # global that each caller was expected to check — and only one of the three
    # did. cmd_upgrade and the .mrpack path through cmd_install_mrpack both
    # write this drop-in and neither checked the flag, so changing
    # BACKUP_SCHEDULE by either route wrote a new schedule that systemd never
    # read. Nothing outside this function can now forget.
    local dropin_dir="/etc/systemd/system/minecraft-backup.timer.d"
    local dropin="${dropin_dir}/schedule.conf"
    if [[ -d /etc/systemd/system ]]; then
        local desired
        desired=$(printf '[Timer]\nOnCalendar=\nOnCalendar=%s' "$BACKUP_SCHEDULE")
        # Write only when the schedule actually moved. Rewriting identical
        # content still bumps mtime and would buy a daemon-reload for a change
        # that was not made.
        if [[ ! -f "$dropin" || "$(cat "$dropin")" != "$desired" ]]; then
            mkdir -p "$dropin_dir"
            printf '%s\n' "$desired" > "$dropin"
            # /run/systemd/system, not `command -v systemctl`: the binary is
            # present in plenty of places systemd is not running (containers),
            # where a reload is a guaranteed error rather than a no-op.
            if [[ -d /run/systemd/system ]]; then
                systemctl daemon-reload 2>/dev/null || true
            fi
        fi
    fi
}

# ── Cleanup registry ───────────────────────────────────────────────────────────

# bash supports exactly ONE EXIT trap, so every cleanup duty has to funnel
# through a single handler. Call sites register/deregister duties here instead
# of calling `trap` themselves — previously each `trap ... EXIT` silently
# replaced the lock-file trap and a later `trap - EXIT` discarded everything,
# leaking /run/minecraft/mc.lock on every install/upgrade/backup (which then
# produced a spurious "Removing stale lock" warning on the next run).

_MC_CLEANUP_TRAP_SET="no"     # whether the single EXIT trap is installed
_MC_CLEANUP_LOCK=""           # lock file to remove on exit ("" = none held)
_MC_CLEANUP_DIRS=()           # staging dirs to remove on exit
_MC_CLEANUP_SAVE_ON="no"      # "yes" = an interrupted backup must re-enable saves

# The single EXIT handler. Duties run most-server-affecting first.
mc_cleanup() {
    local status=$?

    if [[ "$_MC_CLEANUP_SAVE_ON" == "yes" ]]; then
        _MC_CLEANUP_SAVE_ON="no"
        rcon_command "save-on" 2>/dev/null || true
    fi

    local dir
    for dir in ${_MC_CLEANUP_DIRS[@]+"${_MC_CLEANUP_DIRS[@]}"}; do
        [[ -n "$dir" ]] && rm -rf "$dir"
    done
    _MC_CLEANUP_DIRS=()

    if [[ -n "$_MC_CLEANUP_LOCK" ]]; then
        rm -f "$_MC_CLEANUP_LOCK"
        _MC_CLEANUP_LOCK=""
    fi

    return $status
}

# Install the one and only EXIT trap. Idempotent.
mc_cleanup_arm() {
    if [[ "$_MC_CLEANUP_TRAP_SET" != "yes" ]]; then
        _MC_CLEANUP_TRAP_SET="yes"
        trap mc_cleanup EXIT
    fi
    return 0
}

# Register a staging dir for removal if the shell exits before it is committed.
cleanup_register_dir() {
    [[ -n "${1:-}" ]] || return 0
    _MC_CLEANUP_DIRS+=("$1")
    mc_cleanup_arm
}

# Drop a staging dir from the registry once the caller has committed/removed it.
cleanup_unregister_dir() {
    local target="${1:-}" dir
    local -a kept=()
    for dir in ${_MC_CLEANUP_DIRS[@]+"${_MC_CLEANUP_DIRS[@]}"}; do
        [[ "$dir" == "$target" ]] || kept+=("$dir")
    done
    _MC_CLEANUP_DIRS=(${kept[@]+"${kept[@]}"})
    return 0
}

# ── Process lock ───────────────────────────────────────────────────────────────

# Take the exclusive mc lock, or die trying.
#
# RE-ENTRANT. cmd_upgrade holds the lock and then calls cmd_backup, which takes
# it too; a second acquisition from the same process is a no-op rather than a
# self-deadlock. That is what lets cmd_backup lock at all — without it, the
# minecraft-backup.timer could tar /opt/minecraft midway through an install or,
# worse, while cmd_restore was emptying the directory, and the BACKUP_KEEP
# rotation would then prune a good archive in favour of the truncated one.
acquire_lock() {
    [[ "$_MC_CLEANUP_LOCK" == "$LOCK_FILE" ]] && return 0

    mkdir -p "$(dirname "$LOCK_FILE")"

    local attempt held_pid held_cmd
    for attempt in 1 2; do
        # Create-or-fail in a single syscall (noclobber ⇒ O_EXCL). The previous
        # `[[ -f ]]` test followed by a separate write was a TOCTOU: two runs
        # starting together could both see no lock and both proceed.
        if (set -o noclobber
            printf '%s\n%s\n' "$$" "${_MC_CMD:-unknown}" > "$LOCK_FILE") 2>/dev/null
        then
            _MC_CLEANUP_LOCK="$LOCK_FILE"
            mc_cleanup_arm
            return 0
        fi

        # Creation failed, so the file exists: either a live holder, or a lock
        # left behind by a run that was killed before its EXIT trap fired.
        held_pid=$(sed -n '1p' "$LOCK_FILE" 2>/dev/null || true)
        held_cmd=$(sed -n '2p' "$LOCK_FILE" 2>/dev/null || true)

        # NOTE: a recycled PID can make a stale lock look live, which costs a
        # spurious "already running" refusal. That is the safe direction to err
        # in — probing harder (e.g. matching /proc/<pid>/cmdline) risks the
        # opposite mistake, deleting a lock that is genuinely held.
        if [[ -n "$held_pid" ]] && kill -0 "$held_pid" 2>/dev/null; then
            die "Another mc operation is already running: PID $held_pid ($held_cmd). Try again later."
        fi

        [[ "$attempt" -eq 1 ]] || break
        warn "Removing stale lock from PID ${held_pid:-?} (${held_cmd:-unknown})"
        rm -f "$LOCK_FILE"
    done

    die "Could not acquire lock $LOCK_FILE."
}

# ── Java provisioning ──────────────────────────────────────────────────────────

# Ensure a JRE for the given major version is available, installing it via apt
# if missing. Interactive callers (a terminal attached, no --yes) are prompted
# for confirmation; non-interactive callers (Docker entrypoint, --yes) install
# without asking.
#
# Lives here rather than in common.sh: it needs info()/die() and runs apt, so it
# is root-only and has no business being loaded by the unprivileged
# systemd-facing scripts. (The Java *lookup* helpers it builds on are shared and
# do live in common.sh.)
ensure_java() {
    # assume_yes is passed down by the caller, not parsed here; ${2:-no} is just a fallback.
    local required="$1" assume_yes="${2:-no}"
    find_java_binary "$required" &>/dev/null && return 0

    local pkg="openjdk-${required}-jre-headless"

    if [[ "$assume_yes" != "yes" ]]; then
        if [[ ! -t 0 ]]; then
            die "Java ${required} is required but not installed. Re-run with --yes, or install manually: apt install ${pkg}"
        fi
        local confirm
        read -rp "Minecraft requires Java ${required}, which isn't installed. Install ${pkg} now? [y/N] " confirm
        [[ "$confirm" =~ ^[Yy]$ ]] || die "Java ${required} is required. Install manually: apt install ${pkg}"
    fi

    info "Installing ${pkg}..."
    apt-get update -qq && apt-get install -y --no-install-recommends "$pkg" \
        || die "Failed to install ${pkg}. Install manually: apt install ${pkg}"
}

# ── EULA ───────────────────────────────────────────────────────────────────────

# Record acceptance of the Minecraft EULA in $MC_BASE/eula.txt.
#
# Writing `eula=true` accepts a licence agreement on the operator's behalf, so
# it is never implicit. Earlier versions wrote it from init_server_properties()
# as a side effect of installing, which meant nobody was ever asked. Consent now
# comes from exactly one of two places: --accept-eula, or an interactive yes.
#
# Deliberately separate from ensure_java's --yes. That flag consents to
# installing a package; this one consents to a licence. Folding them together
# would let someone accept a legal agreement by asking for a JRE.
#
# No-ops when the EULA is already accepted, so reinstalls and upgrades of an
# existing server never re-prompt.
accept_eula() {
    local accepted="${1:-no}"

    eula_accepted && return 0

    if [[ "$accepted" != "yes" ]]; then
        # Non-interactive with no flag: refuse rather than assume consent.
        if [[ ! -t 0 ]]; then
            die "The Minecraft EULA has not been accepted. Re-run with --accept-eula to accept it (https://www.minecraft.net/eula)."
        fi
        echo "Minecraft's End User Licence Agreement must be accepted before the server"
        echo "can run: https://www.minecraft.net/eula"
        local confirm
        read -rp "Do you accept the Minecraft EULA? [y/N] " confirm
        [[ "$confirm" =~ ^[Yy]$ ]] \
            || die "The Minecraft EULA was not accepted; nothing was installed."
    fi

    mkdir -p "$MC_BASE"
    cat > "$MC_BASE/eula.txt" <<EOF
# Accepted through mc on $(date -u +%Y-%m-%dT%H:%M:%SZ).
# https://www.minecraft.net/eula
eula=true
EOF
    chown "$MC_USER:$MC_USER" "$MC_BASE/eula.txt" 2>/dev/null || true
}

# ── Systemd helpers ────────────────────────────────────────────────────────────

is_running() {
    systemctl is-active --quiet minecraft 2>/dev/null
}

# ── RCON helpers ───────────────────────────────────────────────────────────────

# Send a single RCON command. Returns 1 if RCON is not configured or unavailable.
# load_config first, because the port is derived from SERVER_PORT.
#
# The MC_RCON_TIMEOUT budget matters most on the backup path — save-off/save-all
# run with the world's saves disabled, so a hang there would leave the server
# unable to persist chunks indefinitely.
rcon_command() {
    mc_rcon_available || return 1
    load_config
    mc_rcon_call "$MC_RCON_TIMEOUT" "$@" 2>/dev/null
}

# ── server.properties helpers ──────────────────────────────────────────────────

# server.properties holds the RCON password, so it must never be world-readable.
# It is owned by (and rewritten by) the JVM's own user, so 0640 is the tightest
# mode that still lets the server read and write it.
SPROP_MODE=640

# Apply the intended owner AND mode to server.properties.
#
# The mode alone is not enough. 0640 is readable only because the *owner* is
# $MC_USER; every writer in this file runs as root, so a file that is created
# and then merely chmod'ed ends up 0640 root:root — which the JVM can neither
# read nor write. That failure is near-silent: the server logs a stack trace
# and "Failed to store properties", then falls back to its compiled-in defaults
# (stock port, level-name "world", RCON off), so a server that looks like it
# started fine is running a configuration nobody chose — and, if level-name was
# customised, generating a brand-new empty world beside the real one.
#
# chown tolerates failure so the helper is safe to call from any context; the
# mc commands that reach it already require root.
sprop_secure() {
    local file="${1:-$MC_BASE/server.properties}"
    chown "$MC_USER:$MC_USER" "$file" 2>/dev/null || true
    chmod "$SPROP_MODE" "$file"
}

# Set or replace a key=value in server.properties. Creates the file if absent.
#
# Rewritten in-shell rather than with `sed -i "s|^${key}=.*|${key}=${value}|"`:
# the replacement text was interpolated unescaped, so a value containing the '|'
# delimiter closed the s/// command and the rest was parsed as sed syntax — a
# value of 'x|w /etc/cron.d/pwn|' turned into a sed write-file command executing
# as root. Values here can originate from a pack-supplied server.properties, so
# they are not trusted input.
sprop_set() {
    local key="$1" value="$2"
    local file="$MC_BASE/server.properties"

    if [[ ! -f "$file" ]]; then
        printf '%s=%s\n' "$key" "$value" > "$file"
        sprop_secure "$file"
        return 0
    fi

    local tmp found="no" line
    tmp=$(mktemp "${file}.XXXXXX")

    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ "$line" == "${key}="* ]]; then
            # Collapse duplicate definitions of the same key onto the first.
            [[ "$found" == "yes" ]] && continue
            printf '%s=%s\n' "$key" "$value"
            found="yes"
        else
            printf '%s\n' "$line"
        fi
    done < "$file" > "$tmp"

    [[ "$found" == "yes" ]] || printf '%s=%s\n' "$key" "$value" >> "$tmp"

    # mktemp gives 0600 root:root; carry over the real file's mode and owner so
    # the JVM can still read its own config after the swap.
    chown --reference="$file" "$tmp" 2>/dev/null || true
    chmod --reference="$file" "$tmp" 2>/dev/null || chmod "$SPROP_MODE" "$tmp"
    mv -f "$tmp" "$file"
}

# Keys the system owns. A pack override never gets to set these.
MC_MANAGED_PROPS=(server-port enable-rcon rcon.port rcon.password)

# The value the system wants for a managed key: whatever the live
# server.properties already says, or — when there is no live file yet — the
# correct value derived from config and from whether mc-rcon has provisioned a
# password. Always succeeds; an unknown key yields the empty string.
managed_property_value() {
    local key="$1" current=""

    if [[ -f "$MC_BASE/server.properties" ]]; then
        current=$(mc_sprop_get "$key")
        if [[ -n "$current" ]]; then
            printf '%s' "$current"
            return 0
        fi
    fi

    case "$key" in
        server-port) printf '%s' "${SERVER_PORT:-25565}" ;;
        rcon.port)   printf '%s' "$(mc_rcon_port)" ;;
        enable-rcon)
            # RCON is on only when mc-rcon has generated a password.
            if [[ -f "$PASSWD_FILE" ]]; then printf 'true'; else printf 'false'; fi ;;
        rcon.password)
            if [[ -f "$PASSWD_FILE" ]]; then printf '%s' "$(cat "$PASSWD_FILE")"; fi ;;
    esac
    return 0
}

# Merge an override server.properties into the live one, protecting system-managed keys.
#
# This runs even when there is NO existing server.properties. It used to be
# gated on one existing, which meant a first-time `mc install pack.mrpack`
# rsynced the pack's own server.properties into place verbatim — letting the
# pack set enable-rcon=true with a password of its choosing, and vanilla binds
# RCON to every interface. The managed keys are now always re-applied.
merge_server_properties() {
    local override="$1"
    local dest="$MC_BASE/server.properties"
    [[ -f "$override" ]] || return 0

    # Resolve before the copy: resolution reads the file about to be replaced.
    # An indexed array parallel to MC_MANAGED_PROPS rather than an associative
    # one — `declare -A` requires bash 4 and nothing else in the toolchain does.
    local -a saved=()
    local i
    for (( i=0; i<${#MC_MANAGED_PROPS[@]}; i++ )); do
        saved[i]=$(managed_property_value "${MC_MANAGED_PROPS[i]}")
    done

    # Secure before the sprop_set loop below: those calls carry the mode and
    # owner across their temp-file swap with --reference, so $dest has to be
    # correct first or the pack's ownership would be propagated forward.
    cp "$override" "$dest"
    sprop_secure "$dest"

    # Written unconditionally, empty values included: skipping empties (as this
    # once did) would leave a pack-supplied rcon.password in place.
    for (( i=0; i<${#MC_MANAGED_PROPS[@]}; i++ )); do
        sprop_set "${MC_MANAGED_PROPS[i]}" "${saved[i]}"
    done
}

# Write the initial server.properties (RCON off by default).
#
# Does NOT touch eula.txt — that is accept_eula()'s job, gated on --accept-eula
# or an interactive prompt, and runs before any of this.
init_server_properties() {
    load_config

    # Seeded through managed_property_value so that "the value the system wants
    # for a managed key" has ONE definition, shared with merge_server_properties.
    # The four keys were previously spelled out here, with enable-rcon=false and
    # an empty password hardcoded — so installing mc-rcon first and running
    # `mc install` second produced a server with RCON off despite a provisioned
    # password file, and every RCON-dependent path (the stop countdown, backup's
    # save-off/save-all, `mc rcon`) silently degraded until someone reinstalled
    # mc-rcon to trip its postinst again.
    #
    # Resolved into variables BEFORE the redirection below, which truncates the
    # file these values may be read from.
    local port rcon_enabled rcon_port rcon_password
    port=$(managed_property_value server-port)
    rcon_enabled=$(managed_property_value enable-rcon)
    rcon_port=$(managed_property_value rcon.port)
    rcon_password=$(managed_property_value rcon.password)

    cat > "$MC_BASE/server.properties" <<EOF
server-port=${port}
enable-rcon=${rcon_enabled}
rcon.port=${rcon_port}
rcon.password=${rcon_password}
EOF
    sprop_secure
}

# ── Download helpers ───────────────────────────────────────────────────────────

# Validate a version string (Minecraft, loader, or NeoForge) before it is
# interpolated into a download URL or filename. Real versions look like
# "1.21.4", "24w45a", "21.1.66", "21.4.0-beta", or the literal "latest".
# The charset excludes '/', so a malicious .mrpack cannot smuggle extra URL
# path segments (e.g. "../../evil") or other unexpected characters into a fetch.
validate_version() {
    local ver="$1" label="${2:-version}"
    [[ "$ver" =~ ^[A-Za-z0-9._+-]+$ ]] \
        || die "Invalid ${label} '${ver}': only letters, digits, and . _ + - are allowed."
}

# Verify a downloaded file against an expected hash, deleting it and aborting on
# mismatch. Fail-closed: an empty/absent/"null" expected hash is treated as a
# failure, so an artifact is never installed unverified. $2 selects the
# algorithm (sha1|sha256|sha512).
verify_sha() {
    local file="$1" algo="$2" expected="$3"
    [[ -n "$expected" && "$expected" != "null" ]] \
        || die "Refusing to install $(basename "$file"): no ${algo} checksum available to verify against."

    local actual
    case "$algo" in
        sha1)   actual=$(sha1sum   "$file" | cut -d' ' -f1) ;;
        sha256) actual=$(sha256sum "$file" | cut -d' ' -f1) ;;
        sha512) actual=$(sha512sum "$file" | cut -d' ' -f1) ;;
        *)      die "verify_sha: unknown algorithm '$algo'" ;;
    esac

    if [[ "${actual,,}" != "${expected,,}" ]]; then
        rm -f "$file"
        die "Checksum mismatch for $(basename "$file") (${algo})\n  expected: ${expected}\n  got:      ${actual}"
    fi
    info "Verified $(basename "$file") (${algo})"
}

download_paper() {
    local version="$1" dest="$2"
    local api="https://api.papermc.io/v2/projects/paper"

    if [[ "$version" == "latest" ]]; then
        version=$(curl -sf "${api}" | jq -r '.versions[-1]') \
            || die "Failed to fetch Paper version list."
    fi

    local build_info
    build_info=$(curl -sf "${api}/versions/${version}/builds") \
        || die "Failed to fetch Paper builds for $version."

    local build filename checksum
    build=$(echo "$build_info"    | jq -r '.builds[-1].build')
    filename=$(echo "$build_info" | jq -r '.builds[-1].downloads.application.name')
    checksum=$(echo "$build_info" | jq -r '.builds[-1].downloads.application.sha256')

    info "Downloading Paper $version build $build..."
    curl -sf --proto '=https' -o "$dest" \
        "${api}/versions/${version}/builds/${build}/downloads/${filename}" \
        || die "Failed to download Paper jar."

    verify_sha "$dest" sha256 "$checksum"

    RESOLVED_VERSION="$version"
}

download_vanilla() {
    local version="$1" dest="$2"
    local manifest_url="https://launchermeta.mojang.com/mc/game/version_manifest_v2.json"

    local manifest
    manifest=$(curl -sf "$manifest_url") || die "Failed to fetch Mojang version manifest."

    if [[ "$version" == "latest" ]]; then
        version=$(echo "$manifest" | jq -r '.latest.release')
    fi

    local version_url
    version_url=$(echo "$manifest" | jq -r --arg v "$version" \
        '.versions[] | select(.id==$v) | .url')
    [[ -n "$version_url" ]] || die "Minecraft version '$version' not found in manifest."

    local ver_meta jar_url checksum
    ver_meta=$(curl -sf "$version_url") || die "Failed to fetch version metadata for $version."
    jar_url=$(echo  "$ver_meta" | jq -r '.downloads.server.url')
    checksum=$(echo "$ver_meta" | jq -r '.downloads.server.sha1')

    info "Downloading Vanilla $version..."
    curl -sf --proto '=https' -o "$dest" "$jar_url" || die "Failed to download Vanilla jar."

    verify_sha "$dest" sha1 "$checksum"

    RESOLVED_VERSION="$version"
}

download_fabric() {
    local version="$1" dest="$2"
    local meta="https://meta.fabricmc.net/v2"

    if [[ "$version" == "latest" ]]; then
        version=$(curl -sf "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json" \
            | jq -r '.latest.release') || die "Failed to fetch latest Minecraft version."
    fi

    local loader_version installer_version
    loader_version=$(curl -sf "${meta}/versions/loader/${version}" \
        | jq -r '.[0].loader.version')    || die "Failed to fetch Fabric loader version."
    installer_version=$(curl -sf "${meta}/versions/installer" \
        | jq -r '.[0].version')           || die "Failed to fetch Fabric installer version."

    info "Downloading Fabric $version (loader $loader_version)..."
    # NOTE: Fabric's meta /server/jar endpoint is a dynamically-assembled
    # launcher and publishes no checksum (no sidecar hash, none in the meta
    # JSON), so unlike the other server types this download can only be trusted
    # via TLS. If independent verification is required, switch to the Fabric
    # installer jar from maven.fabricmc.net (which does ship .sha512 sidecars).
    curl -sf --proto '=https' -o "$dest" \
        "${meta}/versions/loader/${version}/${loader_version}/${installer_version}/server/jar" \
        || die "Failed to download Fabric server jar."

    RESOLVED_VERSION="$version"
}

# NeoForge uses an installer JAR, not a ready-to-run server.jar.
# $1 = NeoForge version (or "latest"), $2 = server directory (not a jar path).
install_neoforge() {
    local nf_version="$1" server_dir="$2"
    local base="https://maven.neoforged.net/releases/net/neoforged/neoforge"

    if [[ "$nf_version" == "latest" ]]; then
        local meta
        meta=$(curl -sf "${base}/maven-metadata.xml") \
            || die "Failed to fetch NeoForge metadata."
        nf_version=$(echo "$meta" \
            | grep '<latest>' \
            | sed 's|.*<latest>\(.*\)</latest>.*|\1|')
        [[ -n "$nf_version" ]] || die "Could not determine latest NeoForge version."
    fi

    # Resolved (or manifest-supplied) version is now interpolated into a URL and
    # a filename — reject anything outside the expected version charset.
    validate_version "$nf_version" "NeoForge version"

    local installer_url="${base}/${nf_version}/neoforge-${nf_version}-installer.jar"
    local installer_jar
    installer_jar=$(mktemp --suffix="-neoforge-installer.jar")
    trap 'rm -f "$installer_jar"' RETURN

    info "Downloading NeoForge ${nf_version} installer..."
    curl -sf --proto '=https' -o "$installer_jar" "$installer_url" \
        || die "Failed to download NeoForge installer for version ${nf_version}."

    # This installer jar is executed below, so verify it against the SHA-512
    # published alongside it in the NeoForge Maven repo before running it.
    local expected_sha
    expected_sha=$(curl -sf --proto '=https' "${installer_url}.sha512") \
        || die "Failed to fetch NeoForge installer checksum (${installer_url}.sha512)."
    expected_sha=${expected_sha%%[[:space:]]*}   # strip any trailing filename/newline
    verify_sha "$installer_jar" sha512 "$expected_sha"

    info "Running NeoForge installer (this may take a moment)..."
    local java_bin="java"
    if [[ -n "$JAVA_VERSION" ]]; then
        java_bin=$(find_java_binary "$JAVA_VERSION" 2>/dev/null) || java_bin="java"
    fi

    "$java_bin" -jar "$installer_jar" --installServer "$server_dir" \
        || die "NeoForge installer failed."

    [[ -f "${server_dir}/run.sh" ]] \
        || die "NeoForge installer completed but run.sh was not created."

    chmod +x "${server_dir}/run.sh"
    # Sentinel so start.sh knows to use run.sh instead of server.jar
    touch "${server_dir}/.neoforge"

    RESOLVED_VERSION="$nf_version"
}

# Resolve "latest" to a concrete version WITHOUT downloading anything.
#
# Exists so cmd_upgrade can tell that an upgrade is a no-op *before* paying for
# a backup and the downtime of a stop. Mirrors the resolution each download_*
# function already does internally; those keep doing it for themselves, since
# cmd_install calls them directly.
#
# Prints the resolved version, or nothing if it could not be determined. A
# failure here is deliberately not fatal: callers fall through to the real
# download, which reports the network error properly.
resolve_version() {
    local type="$1" version="$2"

    if [[ "$version" != "latest" ]]; then
        echo "$version"
        return 0
    fi

    case "$type" in
        paper)
            curl -sf "https://api.papermc.io/v2/projects/paper" 2>/dev/null \
                | jq -r '.versions[-1] // empty' 2>/dev/null ;;
        vanilla|fabric)
            curl -sf "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json" 2>/dev/null \
                | jq -r '.latest.release // empty' 2>/dev/null ;;
        neoforge)
            curl -sf "https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml" 2>/dev/null \
                | grep '<latest>' \
                | sed 's|.*<latest>\(.*\)</latest>.*|\1|' ;;
        *) return 1 ;;
    esac
}

# True when re-running an upgrade at the same version would be a genuine no-op.
#
# Only for server types whose artifact is fully determined by the version
# string. Paper publishes new *builds* against an unchanged Minecraft version,
# and Fabric ships new *loader* versions the same way — for those two,
# "same version" does not mean "same jar", and skipping would quietly pin the
# server to a stale build. server.conf records only the Minecraft version, so
# there is nothing cheaper to compare against for them.
version_identifies_artifact() {
    case "$1" in
        vanilla|neoforge) return 0 ;;
        *)                return 1 ;;
    esac
}

download_jar() {
    local type="$1" version="$2" dest="$3"
    validate_version "$version" "Minecraft version"
    RESOLVED_VERSION="$version"
    case "$type" in
        paper)   download_paper   "$version" "$dest" ;;
        vanilla) download_vanilla "$version" "$dest" ;;
        fabric)  download_fabric  "$version" "$dest" ;;
        neoforge)
            # NeoForge installs into the server dir, not a single jar.
            # Callers must use install_neoforge() directly.
            die "Use install_neoforge() for neoforge; download_jar() does not support it."
            ;;
        *) die "Unknown server type '$type'. Valid: vanilla, paper, fabric, neoforge." ;;
    esac
}

# ── Modrinth allowlist ─────────────────────────────────────────────────────────

MRPACK_URL_ALLOWLIST=(
    "cdn.modrinth.com"
    "cdn-raw.modrinth.com"
)

# Returns 0 only for an https URL whose host is exactly an allowlisted CDN.
# The match is anchored at the start of the string: the previous `sed` form
# extracted the host from the first "https://" found *anywhere*, so a value like
# "-Ksomefile#https://cdn.modrinth.com/x" passed the allowlist and was then
# handed to curl as an argv element beginning with '-' (option injection).
# Anchoring rejects that outright; callers additionally pass the value via
# `curl --url` so it can never be parsed as an option.
mrpack_url_allowed() {
    local url="$1"
    [[ "$url" == https://* ]] || return 1
    local rest="${url#https://}"
    local host="${rest%%/*}"     # everything up to the first '/', if any
    local allowed
    for allowed in "${MRPACK_URL_ALLOWLIST[@]}"; do
        [[ "$host" == "$allowed" ]] && return 0
    done
    return 1
}

# ── Staging helpers ────────────────────────────────────────────────────────────

# Create a staging directory on the same filesystem as MC_BASE.
make_staging_dir() {
    mktemp -d "${MC_BASE}.staging.XXXXXX"
}

# Validate a file path taken from an untrusted .mrpack manifest before using it
# to build a destination under the staging dir. A malicious pack can set an
# arbitrary "path" (e.g. "../../../../etc/cron.d/x"); since install runs as root
# and the staged tree is rsynced into MC_BASE, an unchecked path is an arbitrary
# root file write. Reject absolute paths, `~`, backslashes, and any `..`
# component. Returns 0 if the path is safe to use, 1 otherwise.
mrpack_safe_path() {
    local path="$1"
    [[ -n "$path"        ]] || return 1   # empty
    [[ "$path" != /*     ]] || return 1   # absolute
    [[ "$path" != '~'*   ]] || return 1   # home expansion / literal ~ root
    [[ "$path" != *'\'*  ]] || return 1   # backslash (Windows-style separator)
    # Reject any `..` that forms a whole path component (start, middle, or end).
    case "/$path/" in
        */../*) return 1 ;;
    esac
    return 0
}

# Extract one override tree ("overrides" or "server-overrides") out of the pack
# and merge it into the staging dir.
#
# unzip exits 11 when the archive simply contains no matching entries, which is
# the normal case — most packs ship only one of the two trees. Every other
# non-zero status is a real failure and now aborts: the previous `|| true`
# swallowed them all, so a truncated or hostile archive that unzip refused
# midway through left a half-extracted tree that was merged anyway.
mrpack_extract_overrides() {
    local mrpack_file="$1" staging="$2" subdir="$3"
    local outdir="${staging}/_ov_${subdir}" rc=0

    unzip -q -o -d "$outdir" "$mrpack_file" "${subdir}/*" 2>/dev/null || rc=$?
    if [[ "$rc" -ne 0 && "$rc" -ne 11 ]]; then
        die "Failed to extract ${subdir}/ from $(basename "$mrpack_file") (unzip exit ${rc})."
    fi

    if [[ -d "${outdir}/${subdir}" ]]; then
        # Strip any symlinks the pack embedded before merging: a link pointing
        # outside the tree (e.g. -> /etc) would otherwise be copied into MC_BASE
        # and could later be written through. Legitimate packs ship plain files.
        find "${outdir}/${subdir}" -type l -delete
        rsync -a "${outdir}/${subdir}/" "${staging}/"
    fi
    rm -rf "$outdir"
}

# ── mrpack installation ────────────────────────────────────────────────────────

cmd_install_mrpack() {
    # assume_yes is passed down by cmd_install/cmd_upgrade, not parsed here.
    local mrpack_file="$1" assume_yes="${2:-no}"

    # Seed config defaults before anything else.
    #
    # Both callers (cmd_install, cmd_upgrade) already do this, so today the call
    # is redundant — kept because the failure it prevents is disproportionate to
    # its cost. Without it, SERVER_RAM / SERVER_PORT / BACKUP_KEEP /
    # BACKUP_SCHEDULE / JAVA_OPTS are unset and write_config() below aborts
    # under `set -u`, AFTER the pack has been rsynced into MC_BASE — an
    # installed server with no server.conf.
    #
    # It used to be load-bearing: /usr/bin/mc dispatched any cmd_* function by
    # name, so `mc install_mrpack pack.mrpack` entered here directly. The
    # dispatcher now requires mc_register_command, which closed that path.
    # Re-running load_config is harmless either way: the SERVER_TYPE /
    # MINECRAFT_VERSION it seeds are unconditionally replaced from the manifest
    # a few lines below.
    load_config

    [[ -f "$mrpack_file" ]] || die "File not found: $mrpack_file"
    command -v unzip >/dev/null 2>&1 \
        || die "unzip is required for .mrpack support. Install with: apt install unzip"

    # ── Parse manifest ─────────────────────────────────────────────────────────
    local manifest
    manifest=$(unzip -p "$mrpack_file" "modrinth.index.json" 2>/dev/null) \
        || die "Failed to read modrinth.index.json from $mrpack_file"

    local fmt_version
    fmt_version=$(echo "$manifest" | jq -r '.formatVersion')
    [[ "$fmt_version" == "1" ]] \
        || die "Unsupported .mrpack formatVersion: $fmt_version (only version 1 is supported)"

    # ── Resolve version and server type ───────────────────────────────────────
    MINECRAFT_VERSION=$(echo "$manifest" | jq -r '.dependencies.minecraft')

    # VALIDATE IMMEDIATELY, before this value is used for anything at all.
    # It used to be checked only much later, inside download_jar — but it
    # reaches mc_required_java (an arithmetic context, and so a code-execution
    # sink) a few lines below, and on the NeoForge branch it never reached
    # download_jar at all and went straight into write_config's output. Both
    # sinks are hardened in their own right; this is the check that keeps a
    # hostile value out of them in the first place.
    validate_version "$MINECRAFT_VERSION" "Minecraft version"

    local nf_version=""

    if echo "$manifest" | jq -e '.dependencies["fabric-loader"]' >/dev/null 2>&1; then
        SERVER_TYPE="fabric"
    elif echo "$manifest" | jq -e '.dependencies["neoforge"]' >/dev/null 2>&1; then
        SERVER_TYPE="neoforge"
        nf_version=$(echo "$manifest" | jq -r '.dependencies["neoforge"]')
    elif echo "$manifest" | jq -e '.dependencies["forge"]' >/dev/null 2>&1; then
        die "Forge server type is not yet supported."
    elif echo "$manifest" | jq -e '.dependencies["quilt-loader"]' >/dev/null 2>&1; then
        die "Quilt server type is not yet supported."
    else
        SERVER_TYPE="vanilla"
    fi

    JAVA_VERSION=$(mc_required_java "$MINECRAFT_VERSION")

    info "Pack: $SERVER_TYPE $MINECRAFT_VERSION (Java ${JAVA_VERSION}+)"

    # ── Stage everything ───────────────────────────────────────────────────────
    local staging
    staging=$(make_staging_dir)
    cleanup_register_dir "$staging"

    # ── Install server platform ────────────────────────────────────────────────
    if [[ "$SERVER_TYPE" == "neoforge" ]]; then
        install_neoforge "${nf_version:-latest}" "$staging"
    else
        local tmp_jar="${staging}/server.jar"
        download_jar "$SERVER_TYPE" "$MINECRAFT_VERSION" "$tmp_jar"
    fi

    # ── Download mod files from manifest ─────────────────────────────────────
    # Three phases: (1) parse + validate every entry with no network I/O at all,
    # (2) fetch, (3) verify. Validating first means an unsafe pack aborts before
    # a single byte is downloaded.
    #
    # The manifest is parsed by ONE jq pass into TSV rather than 4 jq processes
    # per entry (each of which re-parsed the whole document): ~2.6 s -> ~4 ms on
    # a 200-mod pack.
    local files_tsv
    files_tsv=$(echo "$manifest" | jq -r '
        (.files // [])[]
        | select((.env.server // "required") != "unsupported")
        | [.path, .downloads[0], .hashes.sha512] | @tsv') \
        || die "Failed to parse the file list from modrinth.index.json"

    local -a dl_paths=() dl_urls=() dl_hashes=()
    local path url sha512 dest resolved
    local reused=0

    # NOTE: fed by process substitution, NOT a pipe. A pipe would run the loop
    # body in a subshell, where `die` exits only that subshell and the install
    # would carry on past a rejected path — a fail-open security regression.
    while IFS=$'\t' read -r path url sha512; do
        [[ -n "$path" ]] || continue

        # Reject path traversal before the path is ever used as a destination.
        mrpack_safe_path "$path" \
            || die "Refusing unsafe file path in mrpack manifest: '$path'"

        # Unlike an unsafe path, a non-allowlisted URL is a skip, not an abort.
        if ! mrpack_url_allowed "$url"; then
            warn "Skipping '$path': download URL not in allowlist ($url)"
            continue
        fi

        dest="${staging}/${path}"
        # Defence in depth: confirm the resolved destination stays inside staging.
        resolved=$(realpath -m "$dest")
        case "$resolved/" in
            "$staging"/*) : ;;
            *) die "Refusing path escaping staging dir: '$path'" ;;
        esac

        # Reuse a file the current install already has, byte for byte, instead
        # of fetching it again. A point release of a large pack changes a
        # handful of mods; the manifest's sha512 says exactly which, so the
        # rest need not cross the network. Placed after the path and allowlist
        # checks above so it can only ever write where a download could.
        #
        # Safe by construction: the hash the file is accepted on is the same
        # one verify_sha would check the download against. A mismatch (or a
        # missing hash) just falls through and downloads normally.
        if [[ -n "$sha512" && "$sha512" != "null" && -f "$MC_BASE/$path" ]] \
            && [[ "$(sha512sum "$MC_BASE/$path" 2>/dev/null | cut -d' ' -f1)" == "${sha512,,}" ]]; then
            mkdir -p "$(dirname "$dest")"
            cp "$MC_BASE/$path" "$dest" || die "Failed to reuse existing file: $path"
            reused=$(( reused + 1 ))
            continue
        fi

        dl_paths+=("$path")
        dl_urls+=("$url")
        dl_hashes+=("$sha512")
    done < <(printf '%s\n' "$files_tsv")

    if (( reused > 0 )); then
        info "Reused ${reused} unchanged file(s) from the current install."
    fi

    local total=${#dl_paths[@]}
    if (( total > 0 )); then
        info "Downloading ${total} pack file(s)..."

        # One curl process for the whole set: the CDN connection is reused
        # instead of a fresh TCP+TLS handshake per mod (~30 s of pure handshake
        # on a 200-mod pack). --parallel additionally overlaps transfers.
        # Both flags are feature-detected (curl >= 7.66 / >= 7.67); older curl
        # errors out on unknown options rather than ignoring them, and
        # --parallel ignores -s, so it needs --no-progress-meter to stay quiet.
        local -a curl_args=() curl_parallel=()
        local curl_help
        curl_help=$(curl --help all 2>/dev/null) || curl_help=""
        if grep -q -- '--parallel-max' <<<"$curl_help" \
           && grep -q -- '--no-progress-meter' <<<"$curl_help"; then
            curl_parallel=(--parallel --parallel-max 8 --no-progress-meter)
        fi

        local n
        for (( n=0; n<total; n++ )); do
            mkdir -p "$(dirname "${staging}/${dl_paths[n]}")"
            # --url keeps a hostile URL from ever being read as a curl option.
            curl_args+=(-o "${staging}/${dl_paths[n]}" --url "${dl_urls[n]}")
        done

        curl -sf --proto '=https' ${curl_parallel[@]+"${curl_parallel[@]}"} \
            "${curl_args[@]}" \
            || die "Failed to download one or more pack files."

        # Every downloaded file is verified; verify_sha is fail-closed, so an
        # empty or "null" manifest hash aborts the install.
        for (( n=0; n<total; n++ )); do
            dest="${staging}/${dl_paths[n]}"
            [[ -f "$dest" ]] || die "Download produced no file for '${dl_paths[n]}'."
            verify_sha "$dest" sha512 "${dl_hashes[n]}"
        done
    fi

    # ── Extract overrides (server-overrides/ takes precedence) ────────────────
    # overrides/ first, then server-overrides/ on top.
    mrpack_extract_overrides "$mrpack_file" "$staging" "overrides"
    mrpack_extract_overrides "$mrpack_file" "$staging" "server-overrides"

    # ── Commit to server directory (atomic rename) ────────────────────────────
    mkdir -p "$MC_BASE"

    # Merge server.properties if the pack provided one, protecting system keys.
    # Note there is no "only if one already exists" gate here any more — see
    # merge_server_properties. Removing the pack's copy from staging keeps the
    # rsync below from putting it back.
    if [[ -f "${staging}/server.properties" ]]; then
        merge_server_properties "${staging}/server.properties"
        rm -f "${staging}/server.properties"
    fi

    rsync -a "${staging}/" "${MC_BASE}/"
    cleanup_unregister_dir "$staging"
    rm -rf "$staging"

    # Save manifest for future upgrades.
    echo "$manifest" > "$MRPACK_MANIFEST"

    # write_config() must run before init_server_properties(): the latter
    # calls load_config(), which would otherwise reset SERVER_TYPE/
    # MINECRAFT_VERSION/JAVA_VERSION to defaults right before they're saved,
    # silently recording e.g. "vanilla latest" instead of the pack's
    # resolved "fabric 26.2".
    write_config

    # Ensure system-managed properties are correct after the rsync.
    if [[ ! -f "$MC_BASE/server.properties" ]]; then
        init_server_properties
    fi

    chown -R "$MC_USER:$MC_USER" "$MC_BASE"

    info "Installed $SERVER_TYPE $MINECRAFT_VERSION from $(basename "$mrpack_file")"
    ensure_java "$JAVA_VERSION" "$assume_yes"
}

# ── Artifact installation ──────────────────────────────────────────────────────

# Fetch the configured SERVER_TYPE/MINECRAFT_VERSION into MC_BASE through a
# staging dir, and repoint MINECRAFT_VERSION at the version actually resolved.
#
# Shared by cmd_install and cmd_upgrade, which carried a copy each. The copies
# had already drifted in shape — install used an empty-string `staging` sentinel
# and a trailing `if [[ -n "$staging" ]]` block to reach the rsync that only its
# NeoForge branch needed, while upgrade simply put the rsync in that branch — so
# the two had to be read side by side to confirm they still did the same thing.
#
# Staging exists because both paths write into a live MC_BASE: nothing lands
# there until the artifact is complete and verified. The dir is registered with
# the cleanup registry for the duration, so an abort mid-download does not leave
# it behind.
install_server_artifact() {
    local staging
    staging=$(make_staging_dir)
    cleanup_register_dir "$staging"

    if [[ "$SERVER_TYPE" == "neoforge" ]]; then
        # NeoForge's installer populates a whole tree (run.sh, libraries/), so
        # the staged tree is merged rather than a single jar moved.
        install_neoforge "$MINECRAFT_VERSION" "$staging"
        MINECRAFT_VERSION="$RESOLVED_VERSION"
        rsync -a "${staging}/" "${MC_BASE}/"
    else
        download_jar "$SERVER_TYPE" "$MINECRAFT_VERSION" "${staging}/server.jar"
        MINECRAFT_VERSION="$RESOLVED_VERSION"
        mv "${staging}/server.jar" "$MC_BASE/server.jar"
    fi

    cleanup_unregister_dir "$staging"
    rm -rf "$staging"
}

# ── cmd_install ────────────────────────────────────────────────────────────────

cmd_install() {
    # Parse flags
    # assume_yes is decided here, from --yes/-y below, then passed to callees.
    local mrpack_file="" assume_yes="no" eula_ok="no" force="no"
    load_config  # seed defaults before flag parsing

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --type)         SERVER_TYPE="$2";        shift 2 ;;
            --version)      MINECRAFT_VERSION="$2";  shift 2 ;;
            --yes|-y)       assume_yes="yes";        shift   ;;
            --accept-eula)  eula_ok="yes";           shift   ;;
            --force)        force="yes";             shift   ;;
            *.mrpack)       mrpack_file="$1";        shift   ;;
            --)             shift; break ;;
            -*)             die "Unknown option: $1" ;;
            *)              die "Unexpected argument: $1 (did you mean --type or --version?)" ;;
        esac
    done

    require_root
    acquire_lock

    # Installing over a live server overwrites server.jar and repins the version
    # in server.conf, with none of the protections upgrade has — no backup, no
    # graceful stop. Every other mutating command guards on require_server;
    # this is its inverse. Refuse and point at the command that does it safely.
    if [[ "$force" != "yes" ]] && server_installed; then
        error "A server is already installed in ${MC_BASE}."
        error "To change version:  mc upgrade [--version VER]"
        error "To reinstall over it (overwrites server.jar, no backup taken):"
        die   "                    mc install --force"
    fi

    # Before anything is downloaded: no point fetching a few hundred MB of
    # server jar and mods only to refuse the licence afterwards. Covers the
    # mrpack branch below too.
    accept_eula "$eula_ok"

    if [[ -n "$mrpack_file" ]]; then
        cmd_install_mrpack "$mrpack_file" "$assume_yes"
        return
    fi

    mkdir -p "$MC_BASE"

    install_server_artifact

    JAVA_VERSION=$(mc_required_java "$MINECRAFT_VERSION")
    write_config

    if [[ ! -f "$MC_BASE/server.properties" ]]; then
        init_server_properties
    fi

    # Last, as in cmd_install_mrpack: this ran BEFORE init_server_properties,
    # so the file that call creates was left root-owned and the server came up
    # on defaults. sprop_secure() now covers that file specifically; the order
    # here is what keeps every *other* root-created file correct too.
    chown -R "$MC_USER:$MC_USER" "$MC_BASE"

    info "Installed $SERVER_TYPE $MINECRAFT_VERSION"
    ensure_java "$JAVA_VERSION" "$assume_yes"
    info "Enable and start with: systemctl enable --now minecraft"
}

# ── cmd_upgrade ────────────────────────────────────────────────────────────────

cmd_upgrade() {
    # assume_yes is decided here, from --yes/-y below, then passed to callees.
    local mrpack_file="" new_version="" assume_yes="no" eula_ok="no" force="no"
    load_config

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --version)      new_version="$2"; shift 2 ;;
            --yes|-y)       assume_yes="yes"; shift   ;;
            --accept-eula)  eula_ok="yes";    shift   ;;
            --force)        force="yes";      shift   ;;
            *.mrpack)       mrpack_file="$1"; shift   ;;
            --)             shift; break ;;
            -*)             die "Unknown option: $1" ;;
            *)              die "Unexpected argument: $1" ;;
        esac
    done

    require_root
    require_server
    acquire_lock

    # A no-op on any server installed through mc, which already accepted. It
    # matters for one case: a server whose eula.txt was never written or was
    # set back to false. start.sh refuses to launch that, so upgrading without
    # this check would hand back a server that cannot start.
    accept_eula "$eula_ok"

    # mrpack-based servers require a new mrpack.
    if [[ -f "$MRPACK_MANIFEST" && -z "$mrpack_file" ]]; then
        die "This server was installed from a .mrpack file. Provide a new .mrpack to upgrade: mc upgrade <new.mrpack>"
    fi

    # Decide whether there is anything to do BEFORE the backup and the stop
    # below. Those are the expensive parts — a full archive of the world, then
    # downtime that stop.sh may stretch to five minutes on a populated server —
    # and `mc upgrade` run on a schedule lands here with nothing to do most
    # times it fires.
    if [[ -z "$mrpack_file" && "$force" != "yes" ]] \
        && version_identifies_artifact "$SERVER_TYPE"; then
        local target resolved
        target="${new_version:-$MINECRAFT_VERSION}"
        resolved=$(resolve_version "$SERVER_TYPE" "$target" 2>/dev/null) || resolved=""
        if [[ -n "$resolved" && "$resolved" == "$MINECRAFT_VERSION" ]]; then
            info "Already running ${SERVER_TYPE} ${resolved} — nothing to upgrade."
            info "Reinstall this same version with: mc upgrade --force"
            return 0
        fi
    fi

    # Backup before any changes.
    info "Creating pre-upgrade backup..."
    cmd_backup || die "Pre-upgrade backup failed. Aborting upgrade."

    local was_running=false
    if is_running; then
        was_running=true
        info "Stopping server for upgrade..."
        systemctl stop minecraft
    fi

    if [[ -n "$mrpack_file" ]]; then
        cmd_install_mrpack "$mrpack_file" "$assume_yes"
    else
        [[ -n "$new_version" ]] && MINECRAFT_VERSION="$new_version"

        install_server_artifact

        JAVA_VERSION=$(mc_required_java "$MINECRAFT_VERSION")
        ensure_java "$JAVA_VERSION" "$assume_yes"
        write_config
        chown -R "$MC_USER:$MC_USER" "$MC_BASE"
    fi

    if $was_running; then
        info "Restarting server..."
        systemctl start minecraft
    fi

    info "Upgrade complete."
}

# ── cmd_start ──────────────────────────────────────────────────────────────────

cmd_start() {
    local eula_ok="no"
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --accept-eula) eula_ok="yes"; shift ;;
            --)            shift; break ;;
            -*)            die "Unknown option: $1" ;;
            *)             die "Unexpected argument: $1" ;;
        esac
    done

    require_root
    require_server
    # start.sh refuses to launch a server that has not accepted, so offer the
    # flag at the point of failure. This is also the only way to accept on an
    # existing server: re-running install would re-download the jar.
    accept_eula "$eula_ok"
    # Desired state already reached — say so and succeed. `systemctl start` on
    # an active unit exits 0 for the same reason: a config-management run that
    # asks for a running server and finds one has not failed.
    if is_running; then
        info "Server is already running."
        return 0
    fi
    start_and_verify || return 1
    info "Server started."
}

# Start the unit and report what actually happened.
#
# Shared by cmd_start and cmd_restart because `systemctl start` on a Type=simple
# unit returns as soon as the process is forked — not when the server is up. Both
# commands used to treat that return as success, so a start that refused half a
# second later still printed a cheerful "Server started."/"Server restarted."
#
# Returns 0 only once the server is genuinely running; on failure the reason has
# already been printed.
start_and_verify() {
    # Type=simple rarely fails the start job itself, but when it does (unit
    # masked, jobs cancelled) `set -e` in bin/mc would otherwise abort here with
    # no explanation at all.
    if ! systemctl start minecraft; then
        error "Server failed to start."
        report_unit_failure
        return 1
    fi

    # Wait up to 60 s for the unit to reach active state. Poll at 0.5 s and
    # check *before* the first sleep: the old 5 s-first loop charged a server
    # that was already active a full 5 s of dead time.
    local i
    for (( i=0; i<120; i++ )); do
        # Checked first, and every iteration: start.sh's config refusals exit
        # within milliseconds of the fork, so the unit can already be failed
        # here. Nothing about a missing jar or an unaccepted EULA resolves by
        # waiting, and polling out the full 60 s before saying so buries the
        # one line that explains it.
        if start_failed; then
            error "Server failed to start."
            report_unit_failure
            return 1
        fi
        is_running && settled_running && return 0
        sleep 0.5
    done

    if start_failed; then
        error "Server failed to start."
        report_unit_failure
        return 1
    fi
    is_running && return 0
    error "Server did not reach active state within 60 s."
    error "Check logs with: mc logs"
    return 1
}

# True once the unit has entered a failed state.
start_failed() {
    systemctl is-failed --quiet minecraft 2>/dev/null
}

# Guard against Type=simple's optimism. systemd marks the unit active the moment
# it forks start.sh, so a bare is_running check can catch the window between the
# fork and a config refusal exiting — and report "Server started." about a server
# that is already gone. Re-check after a settle so those exits have landed.
#
# Only paid on the transition to active, not on every poll, and only once: a
# server that clears this is genuinely running, and the 2 s is invisible next to
# the world load that follows.
_MC_START_SETTLE=2
settled_running() {
    sleep "$_MC_START_SETTLE"
    ! start_failed && is_running
}

# Print why the unit failed. start.sh writes its refusals to stderr, which
# systemd routes to the journal, so the reason is already recorded — surface it
# here rather than making the operator go and find it.
report_unit_failure() {
    journalctl -u minecraft -n 15 --no-pager 2>/dev/null \
        || error "Check logs with: mc logs"
}

# ── cmd_stop ───────────────────────────────────────────────────────────────────

cmd_stop() {
    require_root
    # As in cmd_start: already stopped is the requested state, not a failure.
    if ! is_running; then
        info "Server is not running."
        return 0
    fi
    # Graceful warnings are handled by ExecStop=/usr/lib/mc/stop.sh in the unit.
    systemctl stop minecraft
    info "Server stopped."
}

# ── cmd_restart ────────────────────────────────────────────────────────────────

cmd_restart() {
    local eula_ok="no"
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --accept-eula) eula_ok="yes"; shift ;;
            --)            shift; break ;;
            -*)            die "Unknown option: $1" ;;
            *)             die "Unexpected argument: $1" ;;
        esac
    done

    require_root
    require_server
    # Restart starts the server, so it meets the same gate as cmd_start.
    accept_eula "$eula_ok"
    # Stop triggers ExecStop (warnings). Start brings it back up.
    is_running && systemctl stop minecraft
    start_and_verify || return 1
    info "Server restarted."
}

# ── cmd_status ─────────────────────────────────────────────────────────────────

cmd_status() {
    systemctl status minecraft --no-pager
}

# ── cmd_backup ─────────────────────────────────────────────────────────────────

cmd_backup() {
    require_root
    require_server
    # Serialise against install/upgrade/restore. acquire_lock is re-entrant, so
    # cmd_upgrade calling us while it holds the lock is fine; a concurrent
    # minecraft-backup.timer firing during an install is refused, which is the
    # right outcome — the next timer run picks it up.
    acquire_lock
    load_config

    local timestamp backup_file base_name parent_dir
    timestamp=$(date +%Y%m%d-%H%M%S)
    backup_file="${MC_BACKUP}/minecraft-${timestamp}.tar.gz"
    base_name=$(basename "$MC_BASE")
    parent_dir=$(dirname "$MC_BASE")
    mkdir -p "$MC_BACKUP"

    local was_running=false
    if is_running; then
        was_running=true
        rcon_command "$(mc_say_command "[mc] Backup starting — brief lag possible")" 2>/dev/null || true
        rcon_command "save-off"  2>/dev/null || true
        rcon_command "save-all"  2>/dev/null || true
        sleep 3
    fi

    # Ensure save-on is restored even if the script is interrupted. This duty
    # lives in the cleanup registry rather than in a `local` referenced by an
    # ad-hoc EXIT trap: a `local` is out of scope whenever the shell exits from
    # outside this function, so the old trap could silently leave saves off.
    if $was_running; then
        _MC_CLEANUP_SAVE_ON="yes"
        mc_cleanup_arm
    fi

    # Regenerated/derived data — excluded to shrink the archive and shorten the
    # save-off window. libraries/ and mods/ are deliberately NOT excluded:
    # cmd_restore is a plain untar with no re-download step, so dropping them
    # would produce an unbootable restore.
    local -a tar_excludes=(
        "--exclude=${base_name}/logs"
        "--exclude=${base_name}/crash-reports"
        "--exclude=${base_name}/cache"
    )

    info "Creating backup: $backup_file"
    if command -v pigz >/dev/null 2>&1; then
        # Multi-threaded gzip: single-threaded `tar -czf` held the world in
        # save-off for the entire compression, so chunks could not flush.
        #
        # PIPESTATUS rather than the pipeline's own status, which `pipefail`
        # (set by /usr/bin/mc) already makes non-zero on any failure: the point
        # is to name WHICH stage failed in the message below, since "tar failed"
        # and "pigz failed" send an operator to different places.
        #
        # THE TWO BRANCHES ARE IDENTICAL ON PURPOSE. `set -e` would abort the
        # command before the capture on a bare pipeline, and every construct
        # that could stand in for the `if` — `|| true`, a trailing `:` — is
        # itself a command, which overwrites PIPESTATUS with its own result.
        # Wrapping it in `if` is the only form that both survives `set -e` and
        # leaves PIPESTATUS intact for the next statement.
        local -a rc=()
        if tar -c "${tar_excludes[@]}" -C "$parent_dir" "$base_name" | pigz > "$backup_file"; then
            rc=("${PIPESTATUS[@]}")
        else
            rc=("${PIPESTATUS[@]}")
        fi
        if [[ "${rc[0]:-1}" -ne 0 || "${rc[1]:-1}" -ne 0 ]]; then
            rm -f "$backup_file"
            die "Backup failed (tar exit ${rc[0]:-?}, pigz exit ${rc[1]:-?}); backup not created."
        fi
    else
        tar -c "${tar_excludes[@]}" -z -f "$backup_file" -C "$parent_dir" "$base_name" \
            || { rm -f "$backup_file"; die "tar failed; backup not created."; }
    fi

    _MC_CLEANUP_SAVE_ON="no"  # backup succeeded; save-on is handled inline below

    if $was_running; then
        rcon_command "save-on"  2>/dev/null || true
        rcon_command "$(mc_say_command "[mc] Backup complete")" 2>/dev/null || true
    fi

    local keep="${BACKUP_KEEP:-7}"
    [[ "$keep" =~ ^[0-9]+$ ]] || keep=7   # never evaluate an unvalidated string
    if [[ "$keep" -gt 0 ]]; then
        ls -1t "${MC_BACKUP}/minecraft-"*.tar.gz 2>/dev/null \
            | tail -n +"$((keep + 1))" \
            | xargs -r rm --
    fi

    # Deliberately NOT chowned to $MC_USER. Backups are written by root and read
    # only by root (`mc restore`); handing them to the account that runs
    # untrusted mods would let a compromised server rewrite the archive that a
    # later restore extracts as root. $MC_BACKUP is root-owned 0700 to match.
    chmod 600 "$backup_file"
    info "Backup complete: $backup_file"
}

# ── cmd_restore ────────────────────────────────────────────────────────────────

cmd_restore() {
    local archive="${1:-}"
    require_root
    [[ -n "$archive" ]] || die "Usage: mc restore <backup-file>"
    [[ -f "$archive"  ]] || die "Backup file not found: $archive"
    acquire_lock

    if is_running; then
        warn "Stopping server for restore..."
        systemctl stop minecraft
    fi

    # Validate archive members before extracting as root. Reject absolute paths
    # and any `..` traversal, and require every entry to live under the expected
    # top-level dir (basename of MC_BASE). A tampered or hand-crafted archive
    # could otherwise write outside MC_BASE when unpacked into its parent.
    local listing
    listing=$(tar -tzf "$archive" 2>/dev/null) \
        || die "Failed to read archive (not a valid .tar.gz?): $archive"

    # Entry TYPES matter as much as entry names, and a second listing pass is
    # the price of checking them: `tar -t` prints only the member name, so the
    # loop below cannot see a hardlink's target. An entry named
    # 'minecraft/passwd' hardlinked to /etc/shadow satisfies every name check,
    # and the `chown -R` at the end of this function then hands that inode to
    # the minecraft user. Extraction runs as root, which is exempt from
    # fs.protected_hardlinks, so the kernel does not stop it either.
    #
    # `tar -tv` prints the entry type as the first character of the mode column:
    # '-' regular, 'd' directory, 'l' symlink, 'h' hardlink, and s/p/b/c for
    # sockets, FIFOs and device nodes. Only the first two are accepted; a real
    # backup of a server directory contains nothing else.
    local bad_entries
    bad_entries=$(tar -tvzf "$archive" 2>/dev/null | grep -v '^[-d]' | head -n 5) || true
    if [[ -n "$bad_entries" ]]; then
        die "Refusing archive containing links or special files:\n${bad_entries}"
    fi

    local base_name member
    base_name=$(basename "$MC_BASE")
    while IFS= read -r member; do
        [[ -n "$member" ]] || continue
        case "$member" in
            /*|*/../*|../*|*/..)
                die "Refusing archive with unsafe path: '$member'" ;;
        esac
        case "$member" in
            "$base_name"|"$base_name"/|"$base_name"/*) : ;;
            *) die "Refusing archive with unexpected entry '$member' (expected everything under '${base_name}/')." ;;
        esac
    done <<< "$listing"

    info "Restoring from $archive..."
    # Clear existing contents including dotfiles (glob '*' would miss hidden ones).
    find "${MC_BASE:?}" -mindepth 1 -delete
    # --no-same-owner: don't honor uid/gid stored in the archive; we chown below.
    tar --no-same-owner -xzf "$archive" -C "$(dirname "$MC_BASE")" \
        || die "tar extraction failed; server directory may be incomplete."
    chown -R "$MC_USER:$MC_USER" "$MC_BASE"
    info "Restore complete. Start with: mc start"
}

# ── cmd_logs ───────────────────────────────────────────────────────────────────

cmd_logs() {
    exec journalctl -u minecraft -f --no-pager
}

# ── cmd_delete ─────────────────────────────────────────────────────────────────

cmd_delete() {
    require_root
    acquire_lock

    # Nothing to delete is not a failure, but it should not prompt for a
    # destructive confirmation either.
    if ! server_installed; then
        info "No server installed; nothing to delete."
        return 0
    fi

    echo -e "${RED}WARNING: This will permanently delete the server and all its data.${NC}"
    read -rp "Type 'delete' to confirm: " confirm
    [[ "$confirm" == "delete" ]] || die "Confirmation did not match. Aborting."

    if is_running; then
        systemctl stop minecraft
    fi
    systemctl disable minecraft 2>/dev/null || true

    rm -rf "${MC_BASE:?}"
    rm -f  "$SERVER_CONF"
    rm -f  "$PASSWD_FILE"
    rm -f  "$MRPACK_MANIFEST"

    info "Server deleted."
    info "Backups in $MC_BACKUP were preserved."
}

# ── usage ──────────────────────────────────────────────────────────────────────

usage() {
    cat <<'EOF'
mc — Minecraft server lifecycle manager

Usage: mc <command> [options]

Server management:
  install [--type TYPE] [--version VER]   Install the server jar
  install <pack.mrpack>                   Install from a Modrinth modpack
  upgrade [--version VER]                 Upgrade the server jar
  upgrade <new.mrpack>                    Upgrade from a new Modrinth modpack
  delete                                  Permanently remove the server

  --accept-eula   Accept the Minecraft EULA (https://www.minecraft.net/eula).
                  Also accepted by start/restart. Without it, mc asks; a
                  non-interactive run fails.
  --yes / -y      Auto-install a missing Java runtime without prompting.
  --force         install: reinstall over an existing server (overwrites
                  server.jar, takes no backup).
                  upgrade: reinstall even when already at the target version.

  --accept-eula and --yes are both needed for a fully unattended install
  (cloud-init, Ansible, Docker).

Lifecycle:
  start [--accept-eula]                   Start the server
  stop                                    Stop the server (graceful if RCON available)
  restart [--accept-eula]                 Restart the server
  status                                  Show systemd service status

Data management:
  backup                                  Create a timestamped backup
  restore <file>                          Restore from a backup archive

Monitoring:
  logs                                    Follow the server log (journalctl)

Console (requires mc-rcon):
  rcon                                    Open an interactive RCON session
  rcon <command>                          Run a single command and print the response
  Install with: apt install mc-rcon

Server types: vanilla (default), paper, fabric, neoforge
EOF
}
