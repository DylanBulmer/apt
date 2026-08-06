#!/usr/bin/env bash
# Core library sourced by /usr/bin/mc

# Paths, config loading, Java resolution and RCON invocation are shared with the
# systemd-facing scripts (start/stop/reload), which cannot source this file —
# they run unprivileged and need none of the command implementations below.
# shellcheck source=/usr/lib/mc/common.sh
source /usr/lib/mc/common.sh

# ── Output helpers ─────────────────────────────────────────────────────────────

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; NC='\033[0m'

info()  { echo -e "${GREEN}[mc]${NC} $*"; }
warn()  { echo -e "${YELLOW}[mc]${NC} $*" >&2; }
error() { echo -e "${RED}[mc]${NC} $*" >&2; }
die()   { error "$*"; exit 1; }

require_root() {
    [[ $EUID -eq 0 ]] || die "This command must be run as root."
}

require_server() {
    [[ -f "$MC_BASE/server.jar" || -f "$MC_BASE/run.sh" ]] \
        || die "No server installed. Run: mc install"
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

write_config() {
    mkdir -p "$MC_CONFIG"
    cat > "$SERVER_CONF" <<EOF
# mc server configuration
SERVER_TYPE=${SERVER_TYPE}
MINECRAFT_VERSION=${MINECRAFT_VERSION}
JAVA_VERSION=${JAVA_VERSION}
SERVER_RAM=${SERVER_RAM}
SERVER_PORT=${SERVER_PORT}
BACKUP_KEEP=${BACKUP_KEEP}
BACKUP_SCHEDULE=${BACKUP_SCHEDULE}
JAVA_OPTS="${JAVA_OPTS}"
EOF

    # Regenerate backup timer drop-in so daemon-reload picks up schedule changes
    local dropin_dir="/etc/systemd/system/minecraft-backup.timer.d"
    if [[ -d /etc/systemd/system ]]; then
        mkdir -p "$dropin_dir"
        cat > "${dropin_dir}/schedule.conf" <<EOF
[Timer]
OnCalendar=
OnCalendar=${BACKUP_SCHEDULE}
EOF
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

acquire_lock() {
    mkdir -p "$(dirname "$LOCK_FILE")"

    if [[ -f "$LOCK_FILE" ]]; then
        local held_pid held_cmd
        held_pid=$(sed -n '1p' "$LOCK_FILE" 2>/dev/null)
        held_cmd=$(sed -n '2p' "$LOCK_FILE" 2>/dev/null)
        if [[ -n "$held_pid" ]] && kill -0 "$held_pid" 2>/dev/null; then
            die "Another mc operation is already running: PID $held_pid ($held_cmd). Try again later."
        else
            warn "Removing stale lock from PID ${held_pid:-?} (${held_cmd:-unknown})"
        fi
    fi

    printf '%s\n%s\n' "$$" "${_MC_CMD:-unknown}" > "$LOCK_FILE"
    _MC_CLEANUP_LOCK="$LOCK_FILE"
    mc_cleanup_arm
}

# ── Systemd helpers ────────────────────────────────────────────────────────────

is_running() {
    systemctl is-active --quiet minecraft 2>/dev/null
}

# ── RCON helpers ───────────────────────────────────────────────────────────────

# Kept as the name the command implementations (and the mc-rcon plugin) use.
rcon_available() { mc_rcon_available; }

generate_rcon_password() {
    head -c 24 /dev/urandom | base64 | tr '+/' '-_' | tr -d '='
}

# Send a single RCON command. Returns 1 if RCON is not configured or unavailable.
# load_config first, because the port is derived from SERVER_PORT.
#
# The MC_RCON_TIMEOUT budget matters most on the backup path — save-off/save-all
# run with the world's saves disabled, so a hang there would leave the server
# unable to persist chunks indefinitely.
rcon_command() {
    rcon_available || return 1
    load_config
    mc_rcon_call "$MC_RCON_TIMEOUT" "$@" 2>/dev/null
}

# ── server.properties helpers ──────────────────────────────────────────────────

# Set or replace a key=value in server.properties. Creates the file if absent.
sprop_set() {
    local key="$1" value="$2"
    local file="$MC_BASE/server.properties"
    if grep -q "^${key}=" "$file" 2>/dev/null; then
        sed -i "s|^${key}=.*|${key}=${value}|" "$file"
    else
        echo "${key}=${value}" >> "$file"
    fi
}

sprop_get() {
    local key="$1"
    grep "^${key}=" "$MC_BASE/server.properties" 2>/dev/null | cut -d= -f2-
}

# Merge an override server.properties into the live one, protecting system-managed keys.
merge_server_properties() {
    local override="$1"
    local dest="$MC_BASE/server.properties"
    [[ -f "$override" ]] || return 0

    # Keys the system owns — never overwritten by pack overrides
    local -a protected=(server-port enable-rcon rcon.port rcon.password)
    declare -A saved
    for key in "${protected[@]}"; do
        saved["$key"]=$(sprop_get "$key")
    done

    cp "$override" "$dest"

    for key in "${protected[@]}"; do
        local val="${saved[$key]}"
        [[ -n "$val" ]] && sprop_set "$key" "$val"
    done
}

# Write the initial server.properties (RCON off by default).
init_server_properties() {
    load_config
    local rcon_port=$((SERVER_PORT + 10))
    cat > "$MC_BASE/server.properties" <<EOF
server-port=${SERVER_PORT}
enable-rcon=false
rcon.port=${rcon_port}
rcon.password=
EOF
    echo "eula=true" > "$MC_BASE/eula.txt"
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

# ── mrpack installation ────────────────────────────────────────────────────────

cmd_install_mrpack() {
    # assume_yes is passed down by cmd_install/cmd_upgrade, not parsed here.
    local mrpack_file="$1" assume_yes="${2:-no}"

    # Seed config defaults before anything else. cmd_install/cmd_upgrade have
    # already done this, but /usr/bin/mc's plugin fallback dispatches ANY
    # cmd_* function by name, so `mc install_mrpack pack.mrpack` reaches this
    # function directly. Without this call, SERVER_RAM / SERVER_PORT /
    # BACKUP_KEEP / BACKUP_SCHEDULE / JAVA_OPTS are never initialised and
    # write_config() below aborts under `set -u` — after the pack has already
    # been rsynced into MC_BASE, leaving an installed server with no
    # server.conf. Re-running load_config on the normal path is harmless: the
    # values it seeds for SERVER_TYPE / MINECRAFT_VERSION are unconditionally
    # replaced from the manifest a few lines below.
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

        dl_paths+=("$path")
        dl_urls+=("$url")
        dl_hashes+=("$sha512")
    done < <(printf '%s\n' "$files_tsv")

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
    # Extract overrides/ first, then server-overrides/ on top.
    # Strip any symlinks the pack embedded in its overrides before merging them:
    # a symlink pointing outside the tree (e.g. -> /etc) would otherwise be copied
    # into MC_BASE and could later be written through. Legitimate packs ship plain
    # files, not links.
    # The `unzip -l | grep` probes are gone: they re-read the whole archive just
    # to decide whether to re-read it again. Attempting the extraction and then
    # testing for the resulting directory is equivalent and halves the reads.
    unzip -q -o -d "${staging}/_ov" "$mrpack_file" "overrides/*" 2>/dev/null || true
    if [[ -d "${staging}/_ov/overrides" ]]; then
        find "${staging}/_ov/overrides" -type l -delete
        rsync -a "${staging}/_ov/overrides/" "${staging}/"
    fi
    rm -rf "${staging}/_ov"

    unzip -q -o -d "${staging}/_sov" "$mrpack_file" "server-overrides/*" 2>/dev/null || true
    if [[ -d "${staging}/_sov/server-overrides" ]]; then
        find "${staging}/_sov/server-overrides" -type l -delete
        rsync -a "${staging}/_sov/server-overrides/" "${staging}/"
    fi
    rm -rf "${staging}/_sov"

    # ── Commit to server directory (atomic rename) ────────────────────────────
    mkdir -p "$MC_BASE"

    # Merge server.properties if the pack provided one, protecting system keys.
    if [[ -f "${staging}/server.properties" ]]; then
        if [[ -f "$MC_BASE/server.properties" ]]; then
            merge_server_properties "${staging}/server.properties"
            rm -f "${staging}/server.properties"
        fi
        # If no existing server.properties, init_server_properties will create
        # one after the rsync below.
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

# ── cmd_install ────────────────────────────────────────────────────────────────

cmd_install() {
    # Parse flags
    # assume_yes is decided here, from --yes/-y below, then passed to callees.
    local mrpack_file="" assume_yes="no"
    load_config  # seed defaults before flag parsing

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --type)      SERVER_TYPE="$2";        shift 2 ;;
            --version)   MINECRAFT_VERSION="$2";  shift 2 ;;
            --yes|-y)    assume_yes="yes";        shift   ;;
            *.mrpack)    mrpack_file="$1";        shift   ;;
            --)          shift; break ;;
            -*)          die "Unknown option: $1" ;;
            *)           die "Unexpected argument: $1 (did you mean --type or --version?)" ;;
        esac
    done

    require_root
    acquire_lock

    if [[ -n "$mrpack_file" ]]; then
        cmd_install_mrpack "$mrpack_file" "$assume_yes"
        return
    fi

    mkdir -p "$MC_BASE"

    local staging
    staging=$(make_staging_dir)
    cleanup_register_dir "$staging"

    if [[ "$SERVER_TYPE" == "neoforge" ]]; then
        install_neoforge "$MINECRAFT_VERSION" "$staging"
        MINECRAFT_VERSION="$RESOLVED_VERSION"
    else
        local tmp_jar="${staging}/server.jar"
        download_jar "$SERVER_TYPE" "$MINECRAFT_VERSION" "$tmp_jar"
        MINECRAFT_VERSION="$RESOLVED_VERSION"
        mv "$tmp_jar" "$MC_BASE/server.jar"
        cleanup_unregister_dir "$staging"
        rm -rf "$staging"
        staging=""
    fi

    if [[ -n "$staging" ]]; then
        rsync -a "${staging}/" "${MC_BASE}/"
        cleanup_unregister_dir "$staging"
        rm -rf "$staging"
    fi

    JAVA_VERSION=$(mc_required_java "$MINECRAFT_VERSION")
    chown -R "$MC_USER:$MC_USER" "$MC_BASE"
    write_config

    if [[ ! -f "$MC_BASE/server.properties" ]]; then
        init_server_properties
    fi

    systemctl daemon-reload 2>/dev/null || true

    info "Installed $SERVER_TYPE $MINECRAFT_VERSION"
    ensure_java "$JAVA_VERSION" "$assume_yes"
    info "Enable and start with: systemctl enable --now minecraft"
}

# ── cmd_upgrade ────────────────────────────────────────────────────────────────

cmd_upgrade() {
    # assume_yes is decided here, from --yes/-y below, then passed to callees.
    local mrpack_file="" new_version="" assume_yes="no"
    load_config

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --version) new_version="$2"; shift 2 ;;
            --yes|-y)  assume_yes="yes"; shift   ;;
            *.mrpack)  mrpack_file="$1"; shift   ;;
            --)        shift; break ;;
            -*)        die "Unknown option: $1" ;;
            *)         die "Unexpected argument: $1" ;;
        esac
    done

    require_root
    require_server
    acquire_lock

    # mrpack-based servers require a new mrpack.
    if [[ -f "$MRPACK_MANIFEST" && -z "$mrpack_file" ]]; then
        die "This server was installed from a .mrpack file. Provide a new .mrpack to upgrade: mc upgrade <new.mrpack>"
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

        local staging
        staging=$(make_staging_dir)
        cleanup_register_dir "$staging"

        if [[ "$SERVER_TYPE" == "neoforge" ]]; then
            install_neoforge "$MINECRAFT_VERSION" "$staging"
            MINECRAFT_VERSION="$RESOLVED_VERSION"
            rsync -a "${staging}/" "${MC_BASE}/"
            cleanup_unregister_dir "$staging"
            rm -rf "$staging"
        else
            local tmp_jar="${staging}/server.jar"
            download_jar "$SERVER_TYPE" "$MINECRAFT_VERSION" "$tmp_jar"
            MINECRAFT_VERSION="$RESOLVED_VERSION"
            mv "$tmp_jar" "$MC_BASE/server.jar"
            cleanup_unregister_dir "$staging"
            rm -rf "$staging"
        fi

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
    require_root
    require_server
    is_running && die "Server is already running."
    systemctl start minecraft
    # Wait up to 60 s for the unit to reach active state. Poll at 0.5 s and
    # check *before* the first sleep: the old 5 s-first loop charged a server
    # that was already active a full 5 s of dead time.
    local i
    for (( i=0; i<120; i++ )); do
        is_running && { info "Server started."; return 0; }
        sleep 0.5
    done
    is_running && { info "Server started."; return 0; }
    error "Server did not reach active state within 60 s."
    error "Check logs with: mc logs"
    return 1
}

# ── cmd_stop ───────────────────────────────────────────────────────────────────

cmd_stop() {
    require_root
    is_running || die "Server is not running."
    # Graceful warnings are handled by ExecStop=/usr/lib/mc/stop.sh in the unit.
    systemctl stop minecraft
    info "Server stopped."
}

# ── cmd_restart ────────────────────────────────────────────────────────────────

cmd_restart() {
    require_root
    require_server
    # Stop triggers ExecStop (warnings). Start brings it back up.
    is_running && systemctl stop minecraft
    systemctl start minecraft
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
        rcon_command "say [mc] Backup starting — brief lag possible" 2>/dev/null || true
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
        # lib.sh does not set `pipefail` itself, so both stages are checked
        # explicitly via PIPESTATUS instead of relying on the pipeline status.
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
        rcon_command "say [mc] Backup complete" 2>/dev/null || true
    fi

    if [[ "${BACKUP_KEEP:-7}" -gt 0 ]]; then
        ls -1t "${MC_BACKUP}/minecraft-"*.tar.gz 2>/dev/null \
            | tail -n +"$((BACKUP_KEEP + 1))" \
            | xargs -r rm --
    fi

    chown "$MC_USER:$MC_USER" "$backup_file"
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
  install [--type TYPE] [--version VER] [--yes]   Install the server jar
  install <pack.mrpack> [--yes]                   Install from a Modrinth modpack
  upgrade [--version VER] [--yes]                 Upgrade the server jar
  upgrade <new.mrpack> [--yes]                     Upgrade from a new Modrinth modpack
  delete                                          Permanently remove the server

  --yes / -y   Auto-install a missing Java runtime without prompting
               (required in non-interactive contexts, e.g. Docker).

Lifecycle:
  start                                   Start the server
  stop                                    Stop the server (graceful if RCON available)
  restart                                 Restart the server
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
