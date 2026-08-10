#!/usr/bin/env bash
# Drives the INSTALLED /usr/lib/mc/stop.sh end to end, with rcon and sleep mocked.
# Requires: dpkg -i of both packages (tests/run.sh does this).
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../../lib/assert.sh"

# rcon mock: logs each command, answers `list` with a configurable player line.
cat > /usr/local/bin/rcon <<'EOF'
#!/bin/bash
cmd="${*:5}"                       # argv: --password-file F host port cmd...
echo "$cmd" >> "${RCON_LOG:-/tmp/rcon.log}"
[ "$cmd" = "list" ] && echo "${LIST_REPLY:-There are 0 of a max of 20 players online:}"
exit 0
EOF
chmod 755 /usr/local/bin/rcon
# sleep mock: instant, so a 300 s countdown runs in milliseconds.
printf '#!/bin/bash\nexit 0\n' > /usr/local/bin/sleep
chmod 755 /usr/local/bin/sleep

mkdir -p /opt/minecraft /etc/minecraft
: > /etc/minecraft/server.conf     # load_config needs it to exist; ports come from properties
printf 'server-port=25565\nrcon.port=25575\n' > /opt/minecraft/server.properties
echo "pw" > /etc/minecraft/server.passwd

section "players online: full countdown, every step logged"
export RCON_LOG=/tmp/rcon1.log; : > $RCON_LOG
LIST_REPLY="There are 3 of a max of 20 players online: a, b, c" \
    bash /usr/lib/mc/stop.sh > /tmp/out1 2>&1
check_has "start of stop announced"  /tmp/out1 "[mc] Stop requested; asking the server who is online."
check_has "player count + decision"  /tmp/out1 "[mc] 3 player(s) online — triggering the 5-minute countdown."
check_has "5-minute warning logged"  /tmp/out1 "Announced to players: [Server] Shutting down in 5 minutes."
check_has "3-minute warning logged"  /tmp/out1 "Announced to players: [Server] Shutting down in 3 minutes."
check_has "1-minute warning logged"  /tmp/out1 "Announced to players: [Server] Shutting down in 1 minute."
check_has "wait is explained"        /tmp/out1 "[mc] Next warning in 120s."
check_has "final wait explained"     /tmp/out1 "[mc] Final 60s before the server is told to stop."
check_has "stop announced"           /tmp/out1 "[mc] Sending 'stop' to the server."
check_has "flush wait explained"     /tmp/out1 "[mc] Waiting 10s for the server to flush chunks and exit."
check_has "handoff announced"        /tmp/out1 "[mc] Graceful stop finished; handing back to systemd."
check_count "exactly three warnings" /tmp/out1 "Announced to players:" 3
check_has "tellraw sent, not say"    $RCON_LOG 'tellraw @a {"text":"[Server] Shutting down in 5 minutes."}'
check_has "stop command sent"        $RCON_LOG "stop"
check_lacks "no say command"         $RCON_LOG "say ["

section "empty server: no countdown, and it says why"
export RCON_LOG=/tmp/rcon2.log; : > $RCON_LOG
bash /usr/lib/mc/stop.sh > /tmp/out2 2>&1
check_has   "empty-server decision"  /tmp/out2 "[mc] No players online — skipping the countdown and stopping immediately."
check_lacks "no warnings broadcast"  /tmp/out2 "Announced to players:"
check_has   "still stops gracefully" /tmp/out2 "[mc] Sending 'stop' to the server."

section "unparseable list reply: falls back to the conservative path"
export RCON_LOG=/tmp/rcon3.log; : > $RCON_LOG
LIST_REPLY="gibberish from a modded server" bash /usr/lib/mc/stop.sh > /tmp/out3 2>&1
check_has   "unknown count explained" /tmp/out3 "[mc] Player count unavailable — assuming players are online"
check_count "full countdown anyway"   /tmp/out3 "Announced to players:" 3

section "RCON unavailable: says so rather than stopping quietly"
rm -f /etc/minecraft/server.passwd
bash /usr/lib/mc/stop.sh > /tmp/out4 2>&1
check_has "explains the silence"   /tmp/out4 "[mc] RCON unavailable — no in-game warning and no graceful stop"
check_has "tells you how to fix it" /tmp/out4 "[mc] Install mc-rcon to enable the shutdown countdown."
echo "pw" > /etc/minecraft/server.passwd

section "a failed announcement is reported, and the stop still completes"
cat > /usr/local/bin/rcon <<'EOF'
#!/bin/bash
cmd="${*:5}"
[ "$cmd" = "list" ] && { echo "There are 2 of a max of 20 players online: a, b"; exit 0; }
exit 1                             # every other command fails
EOF
chmod 755 /usr/local/bin/rcon
bash /usr/lib/mc/stop.sh > /tmp/out5 2>&1; rc=$?
check_has "failed warning surfaced" /tmp/out5 "[mc] WARNING: could not announce to players:"
check_has "failed stop surfaced"    /tmp/out5 "[mc] WARNING: RCON command failed: stop"
check_has "stop.sh still completes" /tmp/out5 "[mc] Graceful stop finished"
check "exits 0 so systemd proceeds to SIGTERM" 0 "$rc"

report
