#!/usr/bin/env bash
# Called by systemd ExecStart — runs the server in the foreground.
set -euo pipefail

# ── GC flag presets ────────────────────────────────────────────────────────────

FLAGS_G1GC="\
-XX:+UseG1GC \
-XX:+ParallelRefProcEnabled \
-XX:MaxGCPauseMillis=200 \
-XX:+UnlockExperimentalVMOptions \
-XX:+DisableExplicitGC \
-XX:+AlwaysPreTouch \
-XX:G1NewSizePercent=30 \
-XX:G1MaxNewSizePercent=40 \
-XX:G1HeapRegionSize=8M \
-XX:G1ReservePercent=20 \
-XX:G1HeapWastePercent=5 \
-XX:G1MixedGCCountTarget=4 \
-XX:InitiatingHeapOccupancyPercent=15 \
-XX:G1MixedGCLiveThresholdPercent=90 \
-XX:G1RSetUpdatingPauseTimePercent=5 \
-XX:SurvivorRatio=32 \
-XX:+PerfDisableSharedMem \
-XX:MaxTenuringThreshold=1"

FLAGS_ZGC="\
-XX:+UseZGC \
-XX:+ZGenerational \
-XX:-ZUncommit \
-XX:+AlwaysPreTouch \
-XX:+DisableExplicitGC"

# ── Java helpers ───────────────────────────────────────────────────────────────

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

# ── Load config ────────────────────────────────────────────────────────────────

SERVER_DIR="/opt/minecraft"
CONFIG_FILE="/etc/minecraft/server.conf"

SERVER_RAM="4G"
SERVER_FLAGS=""
JAVA_OPTS=""
JAVA_VERSION=""

[[ -f /etc/minecraft/defaults.conf ]] && source /etc/minecraft/defaults.conf
[[ -f "$CONFIG_FILE"               ]] && source "$CONFIG_FILE"

# ── Resolve Java binary ────────────────────────────────────────────────────────

JAVA_BIN="java"
if [[ -n "$JAVA_VERSION" ]]; then
    if found=$(find_java_binary "$JAVA_VERSION" 2>/dev/null); then
        JAVA_BIN="$found"
    else
        echo "WARNING: Java ${JAVA_VERSION} not found; falling back to system java" >&2
    fi
fi

# ── Auto-select GC flags when not explicitly configured ───────────────────────

if [[ -z "$SERVER_FLAGS" ]]; then
    ACTUAL_VER=$(java_major_version "$JAVA_BIN" 2>/dev/null || echo "17")
    if [[ "$ACTUAL_VER" -ge 21 ]]; then
        SERVER_FLAGS="$FLAGS_ZGC"
    else
        SERVER_FLAGS="$FLAGS_G1GC"
    fi
fi

# ── Launch ─────────────────────────────────────────────────────────────────────

cd "$SERVER_DIR"

# NeoForge installs a run.sh instead of a plain server.jar.
if [[ -f run.sh ]]; then
    # Write JVM memory flags into user_jvm_args.txt so run.sh picks them up.
    cat > user_jvm_args.txt <<EOF
-Xmx${SERVER_RAM}
-Xms${SERVER_RAM}
${SERVER_FLAGS}
${JAVA_OPTS}
EOF
    # run.sh uses JAVA_HOME or the system java; override it so we use the
    # correct version selected above.
    export JAVA_HOME
    JAVA_HOME=$(dirname "$(dirname "$JAVA_BIN")")
    exec bash run.sh nogui
fi

if [[ ! -f server.jar ]]; then
    echo "ERROR: server.jar not found in $SERVER_DIR" >&2
    exit 1
fi

# shellcheck disable=SC2086
exec "$JAVA_BIN" -Xmx"${SERVER_RAM}" -Xms"${SERVER_RAM}" \
    ${SERVER_FLAGS} ${JAVA_OPTS} \
    -jar server.jar nogui
