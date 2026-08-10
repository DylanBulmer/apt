#!/usr/bin/env bash
# Wire-level behaviour of the rcon client against a server that frames packets
# the way Minecraft's does.
#
# Minecraft's RconClient does one read() per loop iteration and requires the
# bytes it got to be exactly one packet — `length != bytes_read - 4` makes it
# drop the connection without a word. A client that writes the command and the
# reassembly sentinel back to back hands TCP two small packets with nothing
# between them, they arrive as one segment, and the session dies. The operator
# sees a command succeed and the next one report "connection lost".
#
# The mock below reproduces that check, and pauses before each read so anything
# the client wrote back to back is guaranteed to be waiting together. It is
# compiled rather than scripted because the image has gcc (mc-rcon needs it) but
# no interpreter that can speak a binary protocol.
set -uo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/lib/assert.sh"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"; [[ -n "${MOCK_PID:-}" ]] && kill "$MOCK_PID" 2>/dev/null' EXIT

# ── Build the client under test and the mock server ──────────────────────────

section 'Build'

cat > "$WORK/mock.c" <<'MOCK_EOF'
/* Minimal RCON server with Minecraft's framing rules. Binds an ephemeral port,
 * writes it to argv[1], and logs one line per read to argv[2]. */
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

static FILE *lg;

static int le32(const unsigned char *b)
{
    return (int)((unsigned)b[0] | ((unsigned)b[1] << 8)
               | ((unsigned)b[2] << 16) | ((unsigned)b[3] << 24));
}

static void put32(unsigned char *b, int v)
{
    unsigned u = (unsigned)v;
    b[0] = u & 0xFF; b[1] = (u >> 8) & 0xFF;
    b[2] = (u >> 16) & 0xFF; b[3] = (u >> 24) & 0xFF;
}

static void send_pkt(int fd, int id, int type, const char *payload, int plen)
{
    unsigned char hdr[12];
    put32(hdr,     10 + plen);
    put32(hdr + 4, id);
    put32(hdr + 8, type);
    if (write(fd, hdr, 12) != 12) return;
    if (plen && write(fd, payload, (size_t)plen) != plen) return;
    if (write(fd, "\0\0", 2) != 2) return;
}

int main(int argc, char *argv[])
{
    if (argc < 3) return 2;
    lg = fopen(argv[2], "w");
    setvbuf(lg, NULL, _IOLBF, 0);

    int ls = socket(AF_INET, SOCK_STREAM, 0);
    int one = 1;
    setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
    struct sockaddr_in a = {0};
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    a.sin_port = 0;
    if (bind(ls, (struct sockaddr *)&a, sizeof(a)) < 0) return 2;
    if (listen(ls, 1) < 0) return 2;

    socklen_t alen = sizeof(a);
    getsockname(ls, (struct sockaddr *)&a, &alen);
    FILE *pf = fopen(argv[1], "w");
    fprintf(pf, "%d\n", ntohs(a.sin_port));
    fclose(pf);

    int fd = accept(ls, NULL, NULL);
    if (fd < 0) return 2;

    unsigned char buf[1460];
    char big[4096];
    for (;;) {
        /* Give anything the client wrote back to back time to arrive together,
         * so a client that batches packets is caught every run rather than
         * whenever the scheduler happens to cooperate. */
        struct timespec ts = { .tv_sec = 0, .tv_nsec = 50L * 1000 * 1000 };
        nanosleep(&ts, NULL);

        ssize_t n = read(fd, buf, sizeof(buf));
        if (n <= 0) { fprintf(lg, "EOF\n"); break; }
        fprintf(lg, "READ %zd\n", n);
        if (n < 10) { fprintf(lg, "SHORT\n"); break; }
        int len = le32(buf);
        if (len != (int)n - 4) {
            /* Exactly Minecraft's check: this read holds more (or less) than
             * one whole packet, so the connection is dropped. */
            fprintf(lg, "FRAMING-ERROR len=%d read=%zd\n", len, n);
            break;
        }
        int id   = le32(buf + 4);
        int type = le32(buf + 8);
        int plen = len - 10;
        char payload[1500];
        memcpy(payload, buf + 12, (size_t)plen);
        payload[plen] = '\0';
        fprintf(lg, "PKT id=%d type=%d payload=%s\n", id, type, payload);

        if (type == 3) {                       /* auth */
            send_pkt(fd, id, 2, "", 0);
        } else if (type == 2) {                /* exec */
            if (strcmp(payload, "big") == 0) {
                /* A response at the 4096-byte cap, split across three packets
                 * and written before the sentinel is even read — the case the
                 * sentinel scheme exists for. */
                for (int i = 0; i < 3; i++) {
                    memset(big, "XYZ"[i], sizeof(big));
                    send_pkt(fd, id, 0, big, (int)sizeof(big));
                }
            } else if (plen == 0) {
                send_pkt(fd, id, 0, "", 0);
            } else {
                char out[1600];
                int m = snprintf(out, sizeof(out), "ran %s", payload);
                send_pkt(fd, id, 0, out, m);
            }
        }
    }
    close(fd);
    close(ls);
    fclose(lg);
    return 0;
}
MOCK_EOF

# Same flags the package builds with, so this exercises the shipped binary and
# not a differently-configured one.
if gcc -O2 -Wall -Wextra -std=c11 -D_POSIX_C_SOURCE=200809L -D_DEFAULT_SOURCE \
       -o "$WORK/rcon" "$MC_RCON_PKG/src/rcon.c" 2> "$WORK/cc.log"; then
    check 'rcon.c compiles clean' '' "$(cat "$WORK/cc.log")"
else
    check 'rcon.c compiles' 'ok' "failed: $(cat "$WORK/cc.log")"
    report; exit
fi
gcc -O2 -std=c11 -D_POSIX_C_SOURCE=200809L -D_DEFAULT_SOURCE \
    -o "$WORK/mock" "$WORK/mock.c" || { check 'mock compiles' 'ok' 'failed'; report; exit; }

printf 'testpw\n' > "$WORK/pw"

# start_mock <logfile> — brings up the mock and exports MOCK_PORT.
start_mock() {
    rm -f "$WORK/port"
    "$WORK/mock" "$WORK/port" "$1" &
    MOCK_PID=$!
    local i
    for i in $(seq 1 50); do
        [[ -s "$WORK/port" ]] && break
        sleep 0.1
    done
    MOCK_PORT=$(cat "$WORK/port")
}

# ── A session outlives its first command ─────────────────────────────────────

section 'Interactive session survives consecutive commands'

start_mock "$WORK/log1"
{ printf 'whitelist add A\n'; sleep 0.3
  printf 'whitelist add B\n'; sleep 0.3
  printf 'op C\n';            sleep 0.3
} | "$WORK/rcon" --password-file "$WORK/pw" 127.0.0.1 "$MOCK_PORT" \
      > "$WORK/out1" 2> "$WORK/err1"
rc=$?
wait "$MOCK_PID" 2>/dev/null

check 'client exits 0'            '0' "$rc"
check_lacks 'no connection lost'  "$WORK/err1" 'Connection lost'
# The regression: the command and the sentinel arriving in one read.
check_lacks 'server saw no framing error' "$WORK/log1" 'FRAMING-ERROR'
check_has 'first command answered'  "$WORK/out1" 'ran whitelist add A'
check_has 'second command answered' "$WORK/out1" 'ran whitelist add B'
check_has 'third command answered'  "$WORK/out1" 'ran op C'
# Every packet must land in a read of its own — 3 commands + 3 sentinels + auth.
check_count 'one packet per server read' "$WORK/log1" 'PKT ' 7

# ── Fragmented responses still reassemble ────────────────────────────────────

section 'Multi-packet response reassembly'

start_mock "$WORK/log2"
{ printf 'big\n';   sleep 0.3
  printf 'after\n'; sleep 0.3
} | "$WORK/rcon" --password-file "$WORK/pw" 127.0.0.1 "$MOCK_PORT" \
      > "$WORK/out2" 2> "$WORK/err2"
rc=$?
wait "$MOCK_PID" 2>/dev/null

check 'client exits 0'                    '0' "$rc"
check_lacks 'server saw no framing error' "$WORK/log2" 'FRAMING-ERROR'
# 3 fragments of 4096 reassembled into one response, not truncated to the first.
frag_counts=$(for c in X Y Z; do printf '%s ' "$(tr -cd "$c" < "$WORK/out2" | wc -c | tr -d ' ')"; done)
check 'all three fragments reassembled' '4096 4096 4096 ' "$frag_counts"
# Proof the socket is still correctly positioned: a desynchronised stream would
# hand this command a leftover fragment instead of its own reply.
check_has 'next command answered correctly' "$WORK/out2" 'ran after'

# ── Console output matches mc's ──────────────────────────────────────────────

section 'Message formatting'

# stderr here is a file, not a terminal — the same shape it has under systemd,
# where escape codes would reach the journal as literal bytes.
check_has 'banner carries the [mc] tag' "$WORK/err1" '[mc] Connected to 127.0.0.1:'
check_lacks 'no colour when stderr is not a tty' "$WORK/err1" $'\033['
# Server replies are the payload callers parse; they must stay untagged.
check_lacks 'command output is not tagged' "$WORK/out1" '[mc]'

# Port 1 has nothing listening, so this exercises the failure path without a mock.
"$WORK/rcon" --password-file "$WORK/pw" 127.0.0.1 1 list > /dev/null 2> "$WORK/err3"
check 'unreachable port exits 1' '1' "$?"
check_has 'connect failure is tagged' "$WORK/err3" '[mc] Could not connect to 127.0.0.1:1'

report
