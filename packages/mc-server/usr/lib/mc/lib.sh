#!/usr/bin/env bash
# Shared library sourced by /usr/bin/mc

MC_BASE="${MC_BASE:-/opt/minecraft}"
MC_BACKUP="${MC_BACKUP:-/var/backups/minecraft}"
MC_CONFIG="${MC_CONFIG:-/etc/minecraft}"
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

require_name() {
    local name="${1:-}"
    [[ -n "$name" ]] || die "Server name required."
}

require_server() {
    local name="$1"
    [[ -d "$MC_BASE/$name" ]] || die "Server '$name' not found. Run: mc install $name"
}

# ── Java version helpers ───────────────────────────────────────────────────────

#
# Minecraft version → minimum Java major version mapping:
#
#   < 1.17        Java 8   (pre-modern; Forge 1.12.2, etc.)
#   1.17.x        Java 17  (Mojang requires 16; 16 is EOL so we use LTS 17)
#   1.18 – 1.20.4 Java 17
#   1.20.5+       Java 21  (Mojang hard requirement)
#   1.21+         Java 21
#
mc_required_java() {
    local mc_ver="$1"
    local minor patch
    IFS='.' read -r _ minor patch <<< "$mc_ver"
    minor="${minor:-0}"
    patch="${patch:-0}"

    if   [[ "$minor" -ge 21 ]];                          then echo 21
    elif [[ "$minor" -eq 20 && "$patch" -ge 5 ]];        then echo 21
    elif [[ "$minor" -ge 17 ]];                          then echo 17
    else echo 8
    fi
}

# Return the major version of a given java binary (or the system 'java').
java_major_version() {
    local bin="${1:-java}"
    local raw
    raw=$("$bin" -version 2>&1 | awk -F '"' '/version/ { print $2 }')
    if [[ "$raw" == 1.* ]]; then
        echo "${raw#1.}" | cut -d. -f1   # 1.8.0_xxx → 8
    else
        echo "${raw%%.*}"                 # 21.0.3 → 21
    fi
}

# Find the java binary for a given major version.
# Prints the path to stdout and returns 0 on success, 1 if not found.
find_java_binary() {
    local required="$1"
    local bin

    # Walk update-alternatives registry — matches any provider (OpenJDK, Temurin, Corretto …)
    while IFS= read -r bin; do
        [[ -x "$bin" ]] || continue
        # Path typically contains -<version><non-digit> e.g. java-21-openjdk or temurin-21-amd64
        [[ "$bin" =~ -${required}([^0-9]|$) ]] && { echo "$bin"; return 0; }
    done < <(update-alternatives --list java 2>/dev/null)

    # Direct filesystem search for known JVM install conventions
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
    local name="$1"
    SERVER_TYPE="${DEFAULT_SERVER_TYPE:-paper}"
    MINECRAFT_VERSION="latest"
    JAVA_VERSION=""
    SERVER_RAM="2G"
    SERVER_FLAGS=""
    JAVA_OPTS=""
    SERVER_PORT="25565"
    BACKUP_KEEP="7"

    [[ -f "$MC_CONFIG/defaults.conf"   ]] && source "$MC_CONFIG/defaults.conf"
    [[ -f "$MC_CONFIG/${name}.conf"    ]] && source "$MC_CONFIG/${name}.conf"
}

write_config() {
    local name="$1"
    cat > "$MC_CONFIG/${name}.conf" <<EOF
# mc configuration — ${name}
SERVER_TYPE=${SERVER_TYPE}
MINECRAFT_VERSION=${MINECRAFT_VERSION}
JAVA_VERSION=${JAVA_VERSION}
SERVER_RAM=${SERVER_RAM}
SERVER_PORT=${SERVER_PORT}
BACKUP_KEEP=${BACKUP_KEEP}
JAVA_OPTS="${JAVA_OPTS}"
EOF
}

# ── Systemd helpers ────────────────────────────────────────────────────────────

is_running() {
    systemctl is-active --quiet "minecraft@${1}" 2>/dev/null
}

# ── RCON helpers ───────────────────────────────────────────────────────────────

generate_rcon_password() {
    # 24 random bytes → URL-safe base64 (no padding); 32-char output
    head -c 24 /dev/urandom | base64 | tr '+/' '-_' | tr -d '='
}

# Send a single command via RCON and print the response.
# Silently returns 1 if RCON is not configured, mcrcon is not installed,
# or the server is unreachable.
rcon_command() {
    local name="$1"
    shift
    local passwd_file="$MC_CONFIG/${name}.passwd"
    [[ -f "$passwd_file" ]] || return 1
    command -v mcrcon >/dev/null 2>&1 || return 1
    load_config "$name"
    local port=$((SERVER_PORT + 10))
    local password
    password=$(cat "$passwd_file")
    mcrcon 127.0.0.1 "$port" "$password" "$@"
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

    local build filename
    build=$(echo "$build_info" | jq -r '.builds[-1].build')
    filename=$(echo "$build_info" | jq -r '.builds[-1].downloads.application.name')

    info "Downloading Paper $version build $build..."
    curl -sf -o "$dest" \
        "${api}/versions/${version}/builds/${build}/downloads/${filename}" \
        || die "Failed to download Paper jar."

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

    local version_url jar_url
    version_url=$(echo "$manifest" | jq -r --arg v "$version" \
        '.versions[] | select(.id==$v) | .url') \
        || die "Version $version not found in manifest."
    [[ -n "$version_url" ]] || die "Minecraft version '$version' not found."

    jar_url=$(curl -sf "$version_url" | jq -r '.downloads.server.url') \
        || die "Failed to fetch server jar URL for $version."

    info "Downloading Vanilla $version..."
    curl -sf -o "$dest" "$jar_url" || die "Failed to download Vanilla jar."
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
        | jq -r '.[0].loader.version') || die "Failed to fetch Fabric loader version."
    installer_version=$(curl -sf "${meta}/versions/installer" \
        | jq -r '.[0].version') || die "Failed to fetch Fabric installer version."

    info "Downloading Fabric $version (loader $loader_version)..."
    curl -sf -o "$dest" \
        "${meta}/versions/loader/${version}/${loader_version}/${installer_version}/server/jar" \
        || die "Failed to download Fabric server jar."

    RESOLVED_VERSION="$version"
}

download_jar() {
    local type="$1" version="$2" dest="$3"
    RESOLVED_VERSION="$version"
    case "$type" in
        paper)   download_paper   "$version" "$dest" ;;
        vanilla) download_vanilla "$version" "$dest" ;;
        fabric)  download_fabric  "$version" "$dest" ;;
        *) die "Unknown server type '$type'. Valid: paper, vanilla, fabric." ;;
    esac
}

# ── Commands ───────────────────────────────────────────────────────────────────

cmd_install() {
    require_name "${1:-}"
    require_root
    local name="$1"

    load_config "$name"

    local server_dir="$MC_BASE/$name"
    mkdir -p "$server_dir"

    local tmp_jar
    tmp_jar=$(mktemp --suffix=".jar")
    trap 'rm -f "$tmp_jar"' EXIT

    download_jar "$SERVER_TYPE" "$MINECRAFT_VERSION" "$tmp_jar"

    mv "$tmp_jar" "$server_dir/server.jar"
    trap - EXIT
    chown "$MC_USER:$MC_USER" "$server_dir/server.jar"

    # Derive the required Java version from the resolved MC version
    MINECRAFT_VERSION="$RESOLVED_VERSION"
    JAVA_VERSION=$(mc_required_java "$RESOLVED_VERSION")
    write_config "$name"

    # Generate RCON password if not already present
    local passwd_file="$MC_CONFIG/${name}.passwd"
    if [[ ! -f "$passwd_file" ]]; then
        generate_rcon_password > "$passwd_file"
        chmod 640 "$passwd_file"
        chown root:"$MC_USER" "$passwd_file"
        info "RCON password saved to $passwd_file"
    fi

    # Pre-seed server.properties; the server expands missing keys on first run
    if [[ ! -f "$server_dir/server.properties" ]]; then
        local rcon_pass rcon_port_num
        rcon_pass=$(cat "$passwd_file")
        rcon_port_num=$((SERVER_PORT + 10))
        cat > "$server_dir/server.properties" <<EOF
server-port=${SERVER_PORT}
enable-rcon=true
rcon.port=${rcon_port_num}
rcon.password=${rcon_pass}
EOF
        echo "eula=true" > "$server_dir/eula.txt"
    fi

    chown -R "$MC_USER:$MC_USER" "$server_dir"
    systemctl daemon-reload 2>/dev/null || true

    info "Installed $SERVER_TYPE $RESOLVED_VERSION to $server_dir/server.jar"
    info "Requires Java ${JAVA_VERSION}."

    if ! find_java_binary "$JAVA_VERSION" &>/dev/null; then
        warn "Java ${JAVA_VERSION} not found on this system."
        warn "Install it with: apt install openjdk-${JAVA_VERSION}-jre-headless"
    fi

    info "Enable and start with: systemctl enable --now minecraft@${name}"
}

cmd_update() {
    require_name "${1:-}"
    require_root
    local name="$1"
    require_server "$name"

    local was_running=false
    if is_running "$name"; then
        was_running=true
        warn "Stopping '$name' for update..."
        systemctl stop "minecraft@${name}"
    fi

    cmd_install "$name"
    chown -R "$MC_USER:$MC_USER" "$MC_BASE/$name"

    if $was_running; then
        info "Restarting '$name'..."
        systemctl start "minecraft@${name}"
    fi
}

cmd_start() {
    require_name "${1:-}"
    require_root
    local name="$1"
    require_server "$name"
    is_running "$name" && die "Server '$name' is already running."
    systemctl start "minecraft@${name}"
    info "Started $name."
}

cmd_stop() {
    require_name "${1:-}"
    require_root
    local name="$1"
    require_server "$name"
    is_running "$name" || die "Server '$name' is not running."
    systemctl stop "minecraft@${name}"
    info "Stopped $name."
}

cmd_restart() {
    require_name "${1:-}"
    require_root
    local name="$1"
    require_server "$name"
    systemctl restart "minecraft@${name}"
    info "Restarted $name."
}

cmd_status() {
    require_name "${1:-}"
    local name="$1"
    require_server "$name"
    systemctl status "minecraft@${name}" --no-pager
}

cmd_backup() {
    require_name "${1:-}"
    require_root
    local name="$1"
    require_server "$name"
    load_config "$name"

    local server_dir="$MC_BASE/$name"
    local backup_dir="$MC_BACKUP/$name"
    local timestamp backup_file
    timestamp=$(date +%Y%m%d-%H%M%S)
    backup_file="${backup_dir}/${name}-${timestamp}.tar.gz"

    mkdir -p "$backup_dir"

    local was_running=false
    if is_running "$name"; then
        was_running=true
        rcon_command "$name" "say [mc] Backup starting — brief lag possible" 2>/dev/null || true
        rcon_command "$name" "save-off" 2>/dev/null || true
        rcon_command "$name" "save-all" 2>/dev/null || true
        sleep 3
    fi

    info "Creating backup: $backup_file"
    tar -czf "$backup_file" -C "$MC_BASE" "$name"

    if $was_running; then
        rcon_command "$name" "save-on" 2>/dev/null || true
        rcon_command "$name" "say [mc] Backup complete" 2>/dev/null || true
    fi

    if [[ "${BACKUP_KEEP:-7}" -gt 0 ]]; then
        ls -1t "${backup_dir}/${name}-"*.tar.gz 2>/dev/null \
            | tail -n +"$((BACKUP_KEEP + 1))" \
            | xargs -r rm --
    fi

    chown "$MC_USER:$MC_USER" "$backup_file"
    info "Backup complete: $backup_file"
}

cmd_restore() {
    require_name "${1:-}"
    require_root
    local name="$1"
    local archive="${2:-}"
    [[ -n "$archive" ]] || die "Usage: mc restore <name> <backup-file>"
    [[ -f "$archive"  ]] || die "Backup file not found: $archive"
    require_server "$name"

    if is_running "$name"; then
        warn "Stopping '$name' for restore..."
        systemctl stop "minecraft@${name}"
    fi

    info "Restoring '$name' from $archive..."
    local server_dir="$MC_BASE/$name"
    rm -rf "${server_dir:?}"/*
    tar -xzf "$archive" -C "$MC_BASE"
    chown -R "$MC_USER:$MC_USER" "$server_dir"
    info "Restore complete. Start with: mc start $name"
}

cmd_logs() {
    require_name "${1:-}"
    local name="$1"
    require_server "$name"
    exec journalctl -u "minecraft@${name}" -f --no-pager
}

cmd_delete() {
    require_name "${1:-}"
    require_root
    local name="$1"
    require_server "$name"

    echo -e "${RED}WARNING: This will permanently delete server '$name' and all its data.${NC}"
    read -rp "Type the server name to confirm: " confirm
    [[ "$confirm" == "$name" ]] || die "Confirmation did not match. Aborting."

    if is_running "$name"; then
        systemctl stop "minecraft@${name}"
    fi
    systemctl disable "minecraft@${name}" 2>/dev/null || true

    rm -rf "${MC_BASE:?}/${name}"
    rm -f  "$MC_CONFIG/${name}.conf"
    rm -f  "$MC_CONFIG/${name}.passwd"

    info "Server '$name' deleted."
    info "Backups in $MC_BACKUP/$name were preserved."
}

usage() {
    cat <<'EOF'
mc — Minecraft server lifecycle manager

Usage: mc <command> [arguments]

Server management:
  install <name>             Download/install the server jar and configure
  update <name>              Update server jar to the latest build
  delete <name>              Permanently remove a server

Lifecycle:
  start <name>               Start the server
  stop <name>                Stop the server gracefully
  restart <name>             Restart the server
  status <name>              Show systemd service status

Data management:
  backup <name>              Create a timestamped backup
  restore <name> <file>      Restore from a backup archive

Monitoring:
  logs <name>                Follow the server log (journalctl)

EOF
}
