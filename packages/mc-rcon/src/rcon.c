/*
 * rcon — minimal RCON client for Minecraft servers
 *
 * Packet format (all integers little-endian):
 *   [length:i32][id:i32][type:i32][payload:bytes][pad:\x00\x00]
 *   length = sizeof(id) + sizeof(type) + len(payload) + sizeof(pad)
 *          = 4 + 4 + N + 2  →  PKT_OVERHEAD (10) + N
 *
 *   Auth packet:    type=3, payload=password; server replies id=-1 if denied
 *   Command packet: type=2, payload=command;  server replies with output
 */

#include <arpa/inet.h>
#include <errno.h>
#include <netdb.h>
#include <netinet/in.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

#define RCON_AUTH     3
#define RCON_EXEC     2
#define MAX_PAYLOAD          1446  /* client→server payload limit (Minecraft docs) */
#define MAX_RESPONSE_PAYLOAD 4096  /* server→client payload limit (Minecraft docs) */
#define IO_TIMEOUT_S         10    /* seconds before read/write gives up */
#define PKT_OVERHEAD         10    /* id(4) + type(4) + pad(2) — added to length field */

/* ── Little-endian helpers (work on both LE and BE hosts) ────────────────── */

static uint32_t u32_to_le(uint32_t value)
{
    /* Decompose into individual bytes in little-endian order, then reassemble
     * via memcpy. Direct casting would be undefined behaviour (strict aliasing). */
    uint8_t bytes[4] = {
        value & 0xFF,
        (value >> 8)  & 0xFF,
        (value >> 16) & 0xFF,
        value >> 24
    };
    uint32_t result = 0;
    memcpy(&result, bytes, 4);
    return result;
}
#define le_to_u32 u32_to_le  /* the encoding is symmetric */

/* ── I/O helpers ─────────────────────────────────────────────────────────── */

/*
 * RCON is a plaintext protocol — no TLS layer exists. The password travels
 * unencrypted. Callers must keep the connection on loopback; the mc-rcon
 * shell plugin enforces 127.0.0.1.
 */
// lgtm[cpp/cleartext-transmission]
static int send_all(int fd, const void *buf, size_t remaining)
{
    /* A single write() on a socket may deliver fewer bytes than requested.
     * Loop until every byte has been sent or an error occurs. Retry on EINTR
     * (signal interrupted the syscall before any bytes were transferred). */
    const char *cursor = buf;
    while (remaining) {
        ssize_t written = write(fd, cursor, remaining);
        if (written < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (written == 0) return -1;
        cursor    += written;
        remaining -= (size_t)written;
    }
    return 0;
}

static int recv_all(int fd, void *buf, size_t remaining)
{
    /* Same partial-delivery and EINTR handling as send_all.
     * nread == 0 means the peer closed the connection (EOF). */
    char *cursor = buf;
    while (remaining) {
        ssize_t nread = read(fd, cursor, remaining);
        if (nread < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (nread == 0) return -1;
        cursor    += nread;
        remaining -= (size_t)nread;
    }
    return 0;
}

/* ── RCON packet I/O ─────────────────────────────────────────────────────── */

static int pkt_send(int fd, int32_t id, int32_t type, const char *payload)
{
    size_t raw_len = strlen(payload);
    if (raw_len > MAX_PAYLOAD) {
        fprintf(stderr, "rcon: payload too large (%zu > %d bytes)\n",
                raw_len, MAX_PAYLOAD);
        return -1;
    }

    /* The length field in the protocol counts everything after itself:
     * id(4) + type(4) + payload(N) + pad(2) = PKT_OVERHEAD + payload_len. */
    int32_t payload_len = (int32_t)raw_len;
    int32_t le_len      = (int32_t)u32_to_le((uint32_t)(PKT_OVERHEAD + payload_len));
    int32_t le_id       = (int32_t)u32_to_le((uint32_t)id);
    int32_t le_type     = (int32_t)u32_to_le((uint32_t)type);

    if (send_all(fd, &le_len,  4)                  < 0) return -1;
    if (send_all(fd, &le_id,   4)                  < 0) return -1;
    if (send_all(fd, &le_type, 4)                  < 0) return -1;
    if (send_all(fd, payload, (size_t)payload_len) < 0) return -1;
    if (send_all(fd, "\x00\x00", 2)                < 0) return -1;  /* protocol terminator */
    return 0;
}

/*
 * Receive one packet. On success, *out_payload is heap-allocated (caller frees).
 * Returns 0 on success, -1 on error.
 *
 * Note: the RCON protocol allows a server to split large responses across
 * multiple packets (each capped at MAX_RESPONSE_PAYLOAD bytes). This function
 * reads exactly one packet; callers that need complete output for long commands
 * (e.g. /list on a large server) would need to reassemble continuation packets.
 */
static int pkt_recv(int fd, int32_t *out_id, int32_t *out_type, char **out_payload)
{
    int32_t le_len, le_id, le_type;

    /* Read the length field first so we know how many bytes follow. */
    if (recv_all(fd, &le_len, 4) < 0) return -1;

    int32_t length = (int32_t)le_to_u32((uint32_t)le_len);
    /* Minimum valid packet has an empty payload: id(4)+type(4)+pad(2) = PKT_OVERHEAD.
     * Upper bound uses the server→client limit (4096), not the client→server limit. */
    if (length < PKT_OVERHEAD || length > PKT_OVERHEAD + MAX_RESPONSE_PAYLOAD) {
        fprintf(stderr, "rcon: invalid packet length %d\n", length);
        return -1;
    }

    if (recv_all(fd, &le_id,   4) < 0) return -1;
    if (recv_all(fd, &le_type, 4) < 0) return -1;

    *out_id   = (int32_t)le_to_u32((uint32_t)le_id);
    *out_type = (int32_t)le_to_u32((uint32_t)le_type);

    /* Subtract the fixed overhead to get the number of payload bytes. */
    int32_t data_len = length - PKT_OVERHEAD;
    char *payload = malloc((size_t)data_len + 1);  /* +1 for null terminator */
    if (!payload) return -1;

    if (data_len > 0 && recv_all(fd, payload, (size_t)data_len) < 0) {
        free(payload);
        return -1;
    }
    payload[data_len] = '\0';

    /* Consume the two-byte pad; its value is always \x00\x00 but must be read
     * to keep the stream position in sync for the next packet. */
    char pad[2];
    if (recv_all(fd, pad, 2) < 0) { free(payload); return -1; }

    *out_payload = payload;
    return 0;
}

/* ── Connection helpers ───────────────────────────────────────────────────── */

/*
 * Returns 1 if the address in ai is a loopback address, 0 otherwise.
 * IPv4: anything in 127.0.0.0/8.  IPv6: ::1.
 */
static int is_loopback(const struct addrinfo *ai)
{
    if (ai->ai_family == AF_INET) {
        const struct sockaddr_in *sin = (const struct sockaddr_in *)ai->ai_addr;
        /* ntohl puts the address in host byte order; the top octet identifies
         * the /8 block — the entire 127.x.x.x range is reserved for loopback. */
        return (ntohl(sin->sin_addr.s_addr) >> 24) == 127;
    }
    if (ai->ai_family == AF_INET6) {
        const struct sockaddr_in6 *sin6 = (const struct sockaddr_in6 *)ai->ai_addr;
        return IN6_IS_ADDR_LOOPBACK(&sin6->sin6_addr);
    }
    return 0;
}

static int rcon_connect(const char *host, const char *port)
{
    /* AF_UNSPEC lets getaddrinfo return both IPv4 and IPv6 candidates. */
    struct addrinfo hints = {0}, *res, *candidate;
    hints.ai_family   = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;

    int gai_err = getaddrinfo(host, port, &hints, &res);
    if (gai_err != 0) {
        fprintf(stderr, "rcon: %s: %s\n", host, gai_strerror(gai_err));
        return -1;
    }

    /* Refuse any host that resolves to a non-loopback address.
     * RCON is plaintext; connecting outside loopback would expose the
     * password and all commands on the wire. */
    for (candidate = res; candidate != NULL; candidate = candidate->ai_next) {
        if (!is_loopback(candidate)) {
            fprintf(stderr,
                    "rcon: refusing non-loopback host '%s' — "
                    "RCON is unencrypted and must only be used over loopback "
                    "(127.0.0.1 / ::1)\n", host);
            freeaddrinfo(res);
            return -1;
        }
    }

    /* Iterate through all returned addresses and use the first that connects.
     * This handles dual-stack hosts where e.g. IPv6 is unreachable but IPv4 works. */
    int fd = -1;
    for (candidate = res; candidate != NULL; candidate = candidate->ai_next) {
        fd = socket(candidate->ai_family, candidate->ai_socktype, candidate->ai_protocol);
        if (fd < 0) continue;

        /* Set both timeouts before connecting. Evaluate both calls independently
         * so a failure of the first doesn't silently skip the second. */
        struct timeval tv = { .tv_sec = IO_TIMEOUT_S, .tv_usec = 0 };
        int rcv_err = setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
        int snd_err = setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));
        if (rcv_err < 0 || snd_err < 0) {
            fprintf(stderr, "rcon: warning: could not set I/O timeouts: %s\n",
                    strerror(errno));
        }

        if (connect(fd, candidate->ai_addr, candidate->ai_addrlen) == 0) break;

        close(fd);
        fd = -1;
    }

    freeaddrinfo(res);

    if (fd < 0) {
        fprintf(stderr, "rcon: could not connect to %s:%s\n", host, port);
        return -1;
    }

    return fd;
}

static int rcon_auth(int fd, const char *password)
{
    /* Request ID 1 is an arbitrary correlation tag we chose; the server echoes
     * it back on success. A reply ID of -1 is the protocol's way of signalling
     * that the password was rejected. */
    if (pkt_send(fd, 1, RCON_AUTH, password) < 0) return -1;

    int32_t id, type;
    char *payload;
    if (pkt_recv(fd, &id, &type, &payload) < 0) return -1;
    free(payload);

    /* The server echoes the request ID back on success, or replies -1 on failure.
     * Verify both conditions: the failure sentinel and the expected echo. */
    if (id == -1) {
        fprintf(stderr, "rcon: authentication failed — check password\n");
        return -1;
    }
    if (id != 1) {
        fprintf(stderr, "rcon: unexpected response ID %d during auth\n", id);
        return -1;
    }
    return 0;
}

/* Send a command and return the server's response (caller frees). NULL on error. */
static char *rcon_exec(int fd, const char *cmd)
{
    if (pkt_send(fd, 2, RCON_EXEC, cmd) < 0) return NULL;

    int32_t id, type;
    char *payload;
    if (pkt_recv(fd, &id, &type, &payload) < 0) return NULL;
    return payload;
}

/* ── Helpers for main ────────────────────────────────────────────────────── */

/*
 * Join argv[start..argc-1] into a single space-separated command string.
 * Returns a heap-allocated string (caller frees), or NULL on error.
 */
static char *build_command(int argc, char *argv[], int start)
{
    /* First pass: measure total length needed, enforcing the protocol limit.
     * Check arg_len alone first to prevent size_t wraparound in the addition
     * (arg_len near SIZE_MAX would make total + arg_len + 1 wrap to a small value). */
    size_t total = 0;
    for (int i = start; i < argc; i++) {
        size_t arg_len = strlen(argv[i]);
        if (arg_len > MAX_PAYLOAD || total + arg_len + 1 > MAX_PAYLOAD) {
            fprintf(stderr, "rcon: command too long (> %d bytes)\n", MAX_PAYLOAD);
            return NULL;
        }
        total += arg_len + 1;
    }

    char *cmd = malloc(total + 1);
    if (!cmd) return NULL;

    /* Second pass: copy arguments, inserting spaces between them. */
    char *cursor = cmd;
    for (int i = start; i < argc; i++) {
        if (i > start) *cursor++ = ' ';
        size_t len = strlen(argv[i]);
        memcpy(cursor, argv[i], len);
        cursor += len;
    }
    *cursor = '\0';
    return cmd;
}

static int run_interactive(int fd, const char *host, const char *port)
{
    fprintf(stderr, "Connected to %s:%s — type a command, or Ctrl+D to exit.\n",
            host, port);

    char line[MAX_PAYLOAD + 1];
    for (;;) {
        printf("rcon> ");
        fflush(stdout);

        /* fgets returns NULL on EOF (Ctrl+D) or a read error. */
        if (!fgets(line, (int)sizeof(line), stdin)) {
            putchar('\n');  /* terminal didn't echo a newline for Ctrl+D */
            return 0;
        }

        /* fgets keeps the '\n'; strip it before sending to the server. */
        size_t len = strlen(line);
        if (len && line[len - 1] == '\n') line[--len] = '\0';
        if (!len) continue;
        if (strcmp(line, "exit") == 0 || strcmp(line, "quit") == 0) return 0;

        char *resp = rcon_exec(fd, line);
        if (!resp) {
            fprintf(stderr, "rcon: connection lost\n");
            return 1;
        }
        if (*resp) puts(resp);
        free(resp);
    }
}

/* ── Main ────────────────────────────────────────────────────────────────── */

int main(int argc, char *argv[])
{
    if (argc < 4) {
        fprintf(stderr, "Usage: %s <host> <port> <password> [command ...]\n", argv[0]);
        return 1;
    }

    /* Ignore SIGPIPE so that write() returns -1/EPIPE when the server closes
     * the connection mid-send, rather than killing the process silently. */
    signal(SIGPIPE, SIG_IGN);

    const char *host = argv[1];
    const char *port = argv[2];

    /* Copy the password into a local buffer, then overwrite argv[3] with '*'
     * characters so the plaintext password doesn't remain visible in
     * /proc/<pid>/cmdline or `ps` output after we've read it. */
    char password[MAX_PAYLOAD + 1];
    size_t pw_len = strlen(argv[3]);
    if (pw_len > MAX_PAYLOAD) pw_len = MAX_PAYLOAD;
    memcpy(password, argv[3], pw_len);
    password[pw_len] = '\0';
    memset(argv[3], '*', strlen(argv[3]));

    int fd = rcon_connect(host, port);
    if (fd < 0) return 1;

    int auth_result = rcon_auth(fd, password);

    /* Zero the password buffer as soon as authentication is done so it doesn't
     * linger in stack memory for the rest of the process lifetime. */
    explicit_bzero(password, sizeof(password));

    if (auth_result < 0) {
        close(fd);
        return 1;
    }

    int ret;
    if (argc > 4) {
        char *cmd = build_command(argc, argv, 4);
        if (!cmd) { close(fd); return 1; }

        char *resp = rcon_exec(fd, cmd);
        free(cmd);
        if (!resp) { close(fd); return 1; }
        if (*resp) puts(resp);
        free(resp);
        ret = 0;
    } else {
        ret = run_interactive(fd, host, port);
    }

    close(fd);
    return ret;
}
