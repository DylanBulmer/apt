#!/usr/bin/env bash
# Core library sourced by /usr/bin/mc

MC_BASE="/opt/minecraft"
MC_BACKUP="/var/backups/minecraft"
MC_CONFIG="/etc/minecraft"
SERVER_CONF="$MC_CONFIG/server.conf"
PASSWD_FILE="$MC_CONFIG/server.passwd"
MRPACK_MANIFEST="$MC_CONFIG/server.mrpack.json"
LOCK_FILE="/run/minecraft/mc.lock"
MC_USER="minecraft"

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

# ── Java version helpers ───────────────────────────────────────────────────────

mc_required_java() {
    local mc_ver="$1"
    local major minor patch
    IFS='.' read -r major minor patch <<< "$mc_ver"
    major="${major:-0}"
    minor="${minor:-0}"
    patch="${patch:-0}"

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

# ── Config ─────────────────────────────────────────────────────────────────────

load_config() {
    SERVER_TYPE="${DEFAULT_SERVER_TYPE:-vanilla}"
    MINECRAFT_VERSION="latest"
    JAVA_VERSION=""
    SERVER_RAM="2G"
    SERVER_FLAGS=""
    JAVA_OPTS=""
    SERVER_PORT="25565"
    BACKUP_KEEP="7"
    BACKUP_SCHEDULE="weekly"

    [[ -f "$MC_CONFIG/defaults.conf" ]] && source "$MC_CONFIG/defaults.conf"
    [[ -f "$SERVER_CONF"             ]] && source "$SERVER_CONF"
}

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
    trap 'rm -f "$LOCK_FILE"' EXIT
}

# ── Systemd helpers ────────────────────────────────────────────────────────────

is_running() {
    systemctl is-active --quiet minecraft 2>/dev/null
}

# ── RCON helpers ───────────────────────────────────────────────────────────────

rcon_available() {
    [[ -f "$PASSWD_FILE" ]] && command -v mcrcon >/dev/null 2>&1
}

generate_rcon_password() {
    head -c 24 /dev/urandom | base64 | tr '+/' '-_' | tr -d '='
}

# Send a single RCON command. Returns 1 if RCON is not configured or unavailable.
rcon_command() {
    rcon_available || return 1
    load_config
    local port=$((SERVER_PORT + 10))
    local password
    password=$(cat "$PASSWD_FILE")
    mcrcon 127.0.0.1 "$port" "$password" "$@" 2>/dev/null
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
    curl -sf -o "$dest" \
        "${api}/versions/${version}/builds/${build}/downloads/${filename}" \
        || die "Failed to download Paper jar."

    if [[ -n "$checksum" ]]; then
        local actual
        actual=$(sha256sum "$dest" | cut -d' ' -f1)
        [[ "$actual" == "$checksum" ]] \
            || die "Hash mismatch for Paper jar (expected $checksum, got $actual)"
    fi

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
    curl -sf -o "$dest" "$jar_url" || die "Failed to download Vanilla jar."

    if [[ -n "$checksum" ]]; then
        local actual
        actual=$(sha1sum "$dest" | cut -d' ' -f1)
        [[ "$actual" == "$checksum" ]] \
            || die "Hash mismatch for Vanilla jar (expected $checksum, got $actual)"
    fi

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
    curl -sf -o "$dest" \
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

    local installer_url="${base}/${nf_version}/neoforge-${nf_version}-installer.jar"
    local installer_jar
    installer_jar=$(mktemp --suffix="-neoforge-installer.jar")
    trap 'rm -f "$installer_jar"' RETURN

    info "Downloading NeoForge ${nf_version} installer..."
    curl -sf -o "$installer_jar" "$installer_url" \
        || die "Failed to download NeoForge installer for version ${nf_version}."

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

mrpack_url_allowed() {
    local url="$1"
    local host
    host=$(echo "$url" | sed 's|https://\([^/]*\).*|\1|')
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

# ── mrpack installation ────────────────────────────────────────────────────────

cmd_install_mrpack() {
    local mrpack_file="$1"

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
    trap 'rm -rf "$staging"' EXIT

    # ── Install server platform ────────────────────────────────────────────────
    if [[ "$SERVER_TYPE" == "neoforge" ]]; then
        install_neoforge "${nf_version:-latest}" "$staging"
    else
        local tmp_jar="${staging}/server.jar"
        download_jar "$SERVER_TYPE" "$MINECRAFT_VERSION" "$tmp_jar"
    fi

    # ── Download mod files from manifest ─────────────────────────────────────
    local file_count i
    file_count=$(echo "$manifest" | jq '.files | length')
    for (( i=0; i<file_count; i++ )); do
        local path env_server url sha512
        path=$(      echo "$manifest" | jq -r ".files[$i].path")
        env_server=$(echo "$manifest" | jq -r ".files[$i].env.server // \"required\"")
        [[ "$env_server" == "unsupported" ]] && continue

        url=$(echo "$manifest" | jq -r ".files[$i].downloads[0]")
        if ! mrpack_url_allowed "$url"; then
            warn "Skipping '$path': download URL not in allowlist ($url)"
            continue
        fi

        sha512=$(echo "$manifest" | jq -r ".files[$i].hashes.sha512")

        local dest="${staging}/${path}"
        mkdir -p "$(dirname "$dest")"
        info "Downloading: $path"
        curl -sf -o "$dest" "$url" || die "Failed to download $path"

        local actual_hash
        actual_hash=$(sha512sum "$dest" | cut -d' ' -f1)
        if [[ "$actual_hash" != "$sha512" ]]; then
            rm -f "$dest"
            die "Hash mismatch for $path\n  expected: $sha512\n  got:      $actual_hash"
        fi
    done

    # ── Extract overrides (server-overrides/ takes precedence) ────────────────
    # Extract overrides/ first, then server-overrides/ on top.
    if unzip -l "$mrpack_file" 2>/dev/null | grep -q "overrides/"; then
        unzip -q -o -d "${staging}/_ov" "$mrpack_file" "overrides/*" 2>/dev/null || true
        if [[ -d "${staging}/_ov/overrides" ]]; then
            rsync -a "${staging}/_ov/overrides/" "${staging}/"
            rm -rf "${staging}/_ov"
        fi
    fi
    if unzip -l "$mrpack_file" 2>/dev/null | grep -q "server-overrides/"; then
        unzip -q -o -d "${staging}/_sov" "$mrpack_file" "server-overrides/*" 2>/dev/null || true
        if [[ -d "${staging}/_sov/server-overrides" ]]; then
            rsync -a "${staging}/_sov/server-overrides/" "${staging}/"
            rm -rf "${staging}/_sov"
        fi
    fi

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
    trap - EXIT
    rm -rf "$staging"

    # Ensure system-managed properties are correct after the rsync.
    if [[ ! -f "$MC_BASE/server.properties" ]]; then
        init_server_properties
    fi

    # Save manifest for future upgrades.
    echo "$manifest" > "$MRPACK_MANIFEST"

    write_config
    chown -R "$MC_USER:$MC_USER" "$MC_BASE"

    info "Installed $SERVER_TYPE $MINECRAFT_VERSION from $(basename "$mrpack_file")"
    if ! find_java_binary "$JAVA_VERSION" &>/dev/null; then
        warn "Java ${JAVA_VERSION} not found. Install: apt install openjdk-${JAVA_VERSION}-jre-headless"
    fi
}

# ── cmd_install ────────────────────────────────────────────────────────────────

cmd_install() {
    # Parse flags
    local mrpack_file=""
    load_config  # seed defaults before flag parsing

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --type)    SERVER_TYPE="$2";        shift 2 ;;
            --version) MINECRAFT_VERSION="$2";  shift 2 ;;
            *.mrpack)  mrpack_file="$1";        shift   ;;
            --)        shift; break ;;
            -*)        die "Unknown option: $1" ;;
            *)         die "Unexpected argument: $1 (did you mean --type or --version?)" ;;
        esac
    done

    require_root
    acquire_lock

    if [[ -n "$mrpack_file" ]]; then
        cmd_install_mrpack "$mrpack_file"
        return
    fi

    mkdir -p "$MC_BASE"

    local staging
    staging=$(make_staging_dir)
    trap 'rm -rf "$staging"' EXIT

    if [[ "$SERVER_TYPE" == "neoforge" ]]; then
        install_neoforge "$MINECRAFT_VERSION" "$staging"
        MINECRAFT_VERSION="$RESOLVED_VERSION"
    else
        local tmp_jar="${staging}/server.jar"
        download_jar "$SERVER_TYPE" "$MINECRAFT_VERSION" "$tmp_jar"
        MINECRAFT_VERSION="$RESOLVED_VERSION"
        mv "$tmp_jar" "$MC_BASE/server.jar"
        trap - EXIT
        rm -rf "$staging"
        staging=""
    fi

    if [[ -n "$staging" ]]; then
        rsync -a "${staging}/" "${MC_BASE}/"
        trap - EXIT
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
    if ! find_java_binary "$JAVA_VERSION" &>/dev/null; then
        warn "Java ${JAVA_VERSION} not found. Install: apt install openjdk-${JAVA_VERSION}-jre-headless"
    fi
    info "Enable and start with: systemctl enable --now minecraft"
}

# ── cmd_upgrade ────────────────────────────────────────────────────────────────

cmd_upgrade() {
    local mrpack_file="" new_version=""
    load_config

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --version) new_version="$2"; shift 2 ;;
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
        cmd_install_mrpack "$mrpack_file"
    else
        [[ -n "$new_version" ]] && MINECRAFT_VERSION="$new_version"

        local staging
        staging=$(make_staging_dir)
        trap 'rm -rf "$staging"' EXIT

        if [[ "$SERVER_TYPE" == "neoforge" ]]; then
            install_neoforge "$MINECRAFT_VERSION" "$staging"
            MINECRAFT_VERSION="$RESOLVED_VERSION"
            rsync -a "${staging}/" "${MC_BASE}/"
            trap - EXIT
            rm -rf "$staging"
        else
            local tmp_jar="${staging}/server.jar"
            download_jar "$SERVER_TYPE" "$MINECRAFT_VERSION" "$tmp_jar"
            MINECRAFT_VERSION="$RESOLVED_VERSION"
            mv "$tmp_jar" "$MC_BASE/server.jar"
            trap - EXIT
            rm -rf "$staging"
        fi

        JAVA_VERSION=$(mc_required_java "$MINECRAFT_VERSION")
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
    # Wait up to 60 s for the unit to reach active state.
    local i
    for (( i=0; i<12; i++ )); do
        sleep 5
        is_running && { info "Server started."; return 0; }
    done
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

    local timestamp backup_file
    timestamp=$(date +%Y%m%d-%H%M%S)
    backup_file="${MC_BACKUP}/minecraft-${timestamp}.tar.gz"
    mkdir -p "$MC_BACKUP"

    local was_running=false
    if is_running; then
        was_running=true
        rcon_command "say [mc] Backup starting — brief lag possible" 2>/dev/null || true
        rcon_command "save-off"  2>/dev/null || true
        rcon_command "save-all"  2>/dev/null || true
        sleep 3
    fi

    # Ensure save-on is restored even if the script is interrupted.
    local save_on_needed=$was_running
    trap '[[ "$save_on_needed" == "true" ]] && rcon_command "save-on" 2>/dev/null || true' EXIT

    info "Creating backup: $backup_file"
    tar -czf "$backup_file" -C "$(dirname "$MC_BASE")" "$(basename "$MC_BASE")" \
        || die "tar failed; backup not created."

    save_on_needed=false  # backup succeeded; clear the trap duty
    trap - EXIT

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

    info "Restoring from $archive..."
    rm -rf "${MC_BASE:?}"/*
    tar -xzf "$archive" -C "$(dirname "$MC_BASE")"
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
  install [--type TYPE] [--version VER]   Install the server jar
  install <pack.mrpack>                   Install from a Modrinth modpack
  upgrade [--version VER]                 Upgrade the server jar
  upgrade <new.mrpack>                    Upgrade from a new Modrinth modpack
  delete                                  Permanently remove the server

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
