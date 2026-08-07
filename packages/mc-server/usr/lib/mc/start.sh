#!/usr/bin/env bash
# Called by systemd ExecStart — runs the server in the foreground.
set -euo pipefail

# ── GC flag presets ────────────────────────────────────────────────────────────
#
# Java 8  → FLAGS_G1GC_JAVA8   Aikar's G1GC; UnlockExperimentalVMOptions is
#                               required because several tuning flags were still
#                               marked experimental in Java 8.
# Java 17 → FLAGS_G1GC_JAVA17  Same Aikar flags without UnlockExperimentalVMOptions;
#                               those options became stable in Java 9–11 and the
#                               unlock flag could silently enable other experimental
#                               behaviour in newer runtimes.
# Java 21 → FLAGS_ZGC           Generational ZGC. See FLAG_ZGENERATIONAL below for
# Java 25 →                     why the generational switch is applied by version
#                               rather than folded into this preset.

# ── Exit codes ─────────────────────────────────────────────────────────────────
#
# EX_CONFIG (78, from sysexits(3)) means "an operator has to fix something before
# this unit can start" — EULA not accepted, unreadable server.properties, missing
# server.jar. It is distinct from the JVM's own exit codes on purpose.
#
# The unit used to carry SuccessExitStatus=0 1, which papered over the two cases
# with one rule: the gates below exit non-zero, and so does a crashing JVM, so
# declaring 1 a success silenced BOTH. A server that died of a real fault was
# recorded as a clean stop, Restart=on-failure never fired, and `systemctl
# is-failed` stayed quiet. Now the gates use a code of their own, the unit maps
# it to RestartPreventExitStatus=, and every other non-zero exit is a genuine
# failure that systemd reports and restarts.
EX_CONFIG=78

FLAGS_G1GC_JAVA8="\
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

FLAGS_G1GC_JAVA17="\
-XX:+UseG1GC \
-XX:+ParallelRefProcEnabled \
-XX:MaxGCPauseMillis=200 \
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
-XX:-ZUncommit \
-XX:+AlwaysPreTouch \
-XX:+DisableExplicitGC"

# Applied only to Java 21–23, never folded into FLAGS_ZGC above.
#
# -XX:+ZGenerational arrived experimental in Java 21, became the ZGC default in
# Java 23, and was REMOVED in Java 24. On Java 24+ it produces:
#
#   OpenJDK 64-Bit Server VM warning: Ignoring option ZGenerational;
#   support was removed in 24.0
#
# Harmless in itself — generational mode is the default there anyway, so the
# resulting GC behaviour is identical. It matters because obsolete VM options do
# not stay ignored: the JVM's usual path is ignored → deprecated → rejected, and
# an option that reaches "Unrecognized VM option" makes the JVM refuse to start
# at all. That would take the server down on a routine `apt upgrade` of the JRE,
# with a failure that looks nothing like its cause. Passing the flag only where
# it exists costs nothing and removes that.
FLAG_ZGENERATIONAL="-XX:+ZGenerational"

# ── Load config ────────────────────────────────────────────────────────────────

# Paths, load_config() and the Java resolution helpers are shared with the mc
# CLI. This file deliberately does NOT source lib.sh: that pulls in every
# command implementation, and this runs unprivileged under ProtectSystem=strict
# as systemd's ExecStart=.
# shellcheck source=/usr/lib/mc/common.sh
source /usr/lib/mc/common.sh

load_config
SERVER_DIR="$MC_BASE"

# ── EULA gate ──────────────────────────────────────────────────────────────────
#
# Checked here, before the JVM is launched, for two reasons. The server's own
# check writes a default eula.txt and exits almost immediately, which systemd
# reports as a start followed by a puzzling stop with nothing useful in the
# journal. And `systemctl start minecraft` bypasses the mc CLI entirely, so
# this is the only thing standing between a stray start and a server running
# under a licence nobody accepted.
if ! eula_accepted; then
    # Not "re-run mc install": that re-downloads the server jar. `mc start`
    # accepts the flag precisely so an existing server has a cheap way back.
    echo "ERROR: the Minecraft EULA has not been accepted." >&2
    echo "       https://www.minecraft.net/eula" >&2
    echo "       Accept it with: mc start --accept-eula" >&2
    echo "       or set eula=true in ${SERVER_DIR}/eula.txt" >&2
    exit "$EX_CONFIG"
fi

# ── server.properties access gate ──────────────────────────────────────────────
#
# The JVM treats an unreadable server.properties as an absent one: it logs a
# stack trace, reports "Failed to store properties to file", and then carries on
# with its compiled-in defaults — stock port, RCON off, level-name "world". The
# server appears to start normally while ignoring every setting the operator
# configured, and if level-name was customised it generates a new empty world
# next to the real one. Fail here instead, where the reason is legible.
#
# The usual cause is a root-owned file: 0640 is readable only because the owner
# is the service account, and editing it as root with an editor that writes and
# renames (sed -i, some vim configs) replaces it with a root-owned inode.
#
# An absent file is fine — that is a first boot, and the server writes its own.
_MC_SPROPS="${SERVER_DIR}/server.properties"
if [[ -e "$_MC_SPROPS" ]] && { [[ ! -r "$_MC_SPROPS" ]] || [[ ! -w "$_MC_SPROPS" ]]; }; then
    echo "ERROR: ${_MC_SPROPS} is not readable and writable by $(id -un)." >&2
    echo "       The server would silently start on default settings." >&2
    echo "       Fix with: chown ${MC_USER}:${MC_USER} ${_MC_SPROPS} && chmod 640 ${_MC_SPROPS}" >&2
    exit "$EX_CONFIG"
fi
unset _MC_SPROPS

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
    # java_major_version parses `java -version` output, so it can hand back
    # something non-numeric if the runtime formats its banner unexpectedly.
    # The comparisons below are arithmetic contexts, which evaluate their
    # operand as an expression — feed them only digits.
    [[ "$ACTUAL_VER" =~ ^[0-9]+$ ]] || ACTUAL_VER=17
    if   [[ "$ACTUAL_VER" -ge 21 ]]; then
        SERVER_FLAGS="$FLAGS_ZGC"           # Java 21, 25
        # Java 24 removed the flag; 23 already defaults to it. Only 21–23 need
        # it stated, and only 21–22 actually change behaviour because of it.
        if [[ "$ACTUAL_VER" -le 23 ]]; then
            SERVER_FLAGS="$SERVER_FLAGS $FLAG_ZGENERATIONAL"
        fi
    elif [[ "$ACTUAL_VER" -ge 17 ]]; then
        SERVER_FLAGS="$FLAGS_G1GC_JAVA17"   # Java 17
    else
        SERVER_FLAGS="$FLAGS_G1GC_JAVA8"    # Java 8
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
    echo "       Install one with: mc install" >&2
    exit "$EX_CONFIG"
fi

# shellcheck disable=SC2086
exec "$JAVA_BIN" -Xmx"${SERVER_RAM}" -Xms"${SERVER_RAM}" \
    ${SERVER_FLAGS} ${JAVA_OPTS} \
    -jar server.jar nogui
