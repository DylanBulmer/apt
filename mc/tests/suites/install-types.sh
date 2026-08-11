#!/usr/bin/env bash
# One real `mc install` of $MC_TYPE against the live upstream API.
#
# THE ONLY THING IN THE TREE THAT CATCHES UPSTREAM DRIFT. Every API mc talks to
# has changed shape at least once — PaperMC's v2 was sunset outright and v3
# lives on a different host with a different response shape — and the offline
# fixtures the cargo tests use cannot notice. That makes this suite slow and
# occasionally red for reasons outside this repository, which is the point:
# run it on a schedule, and read a failure here as "upstream moved" before
# reading it as "we broke something".
#
# Writes a one-line verdict to /res/$MC_TYPE.txt so run.sh can run the four
# types as four parallel containers.
set -uo pipefail

TYPE="${MC_TYPE:-vanilla}"
RESULT="/res/${TYPE}.txt"
mkdir -p /res

fail() { printf 'FAIL %-9s %s\n' "$TYPE" "$*" > "$RESULT"; exit 1; }
pass() { printf 'ok   %-9s %s\n' "$TYPE" "$*" > "$RESULT"; exit 0; }

out=$(mc install --type "$TYPE" --accept-eula --yes 2>&1) \
    || fail "install failed: $(printf '%s' "$out" | tail -3 | tr '\n' ' ')"

# ── The artifact landed ────────────────────────────────────────────────────
# NeoForge installs a run.sh tree rather than a single jar, so both count.
if [[ ! -s /opt/minecraft/server.jar && ! -s /opt/minecraft/run.sh ]]; then
    fail "no server artifact in /opt/minecraft"
fi

# ── The version was resolved and PINNED ────────────────────────────────────
# "latest" left in the config means a later upgrade cannot tell what moved.
version=$(grep -E '^version' /etc/minecraft/config.toml | head -1 | sed 's/.*= *"\(.*\)".*/\1/')
[[ -n "$version" && "$version" != "latest" ]] \
    || fail "version not pinned in config.toml (got '${version}')"

# ── server.properties exists, complete, and correctly owned ────────────────
# `--initSettings` is what writes it before any world is generated, which is the
# only window in which level-seed is still meaningful. A failure there is a
# warning rather than an abort, so it is asserted here rather than assumed.
[[ -f /opt/minecraft/server.properties ]] \
    || fail "server.properties was not written"
grep -q '^level-seed=' /opt/minecraft/server.properties \
    || fail "server.properties is not fully populated (no level-seed)"

owner=$(stat -c '%U:%G' /opt/minecraft/server.properties)
mode=$(stat -c '%a' /opt/minecraft/server.properties)
[[ "$owner" == "minecraft:minecraft" ]] \
    || fail "server.properties owner is ${owner}, not minecraft:minecraft"
[[ "$mode" == "640" ]] \
    || fail "server.properties mode is ${mode}, not 640"

# ── The whole tree belongs to the service account ──────────────────────────
# A root-owned file anywhere in here is a file the JVM cannot write.
stray=$(find /opt/minecraft ! -user minecraft -print -quit)
[[ -z "$stray" ]] || fail "not owned by minecraft: ${stray}"

# ── Nothing was left behind ────────────────────────────────────────────────
leftover=$(find /opt -maxdepth 1 -name '.mc-staging-*' -print -quit)
[[ -z "$leftover" ]] || fail "staging directory left behind: ${leftover}"

# ── The runtime chosen is one that exists ──────────────────────────────────
java_major=$(grep -E '^version' /etc/minecraft/config.toml | sed -n '2p' | sed 's/[^0-9]//g')
if [[ -n "$java_major" ]]; then
    ls -d /usr/lib/jvm/*"-${java_major}-"* >/dev/null 2>&1 \
        || fail "config selects Java ${java_major}, which is not installed"
fi

pass "$version"
