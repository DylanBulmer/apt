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
#include <fcntl.h>
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

    /* Build the whole packet in one buffer and send it with a single write
     * loop. Writing the header, payload, and pad as separate send_all() calls
     * lets them land as separate TCP segments; some server RCON parsers
     * (observed against modern vanilla Minecraft servers) mis-frame the
     * following packet when a message arrives split like that, breaking the
     * connection right after the first exchange. */
    size_t total_len = (size_t)PKT_OVERHEAD + (size_t)payload_len;
    char *buf = malloc(4 + total_len);
    if (!buf) return -1;
    memcpy(buf,      &le_len,  4);
    memcpy(buf + 4,  &le_id,   4);
    memcpy(buf + 8,  &le_type, 4);
    memcpy(buf + 12, payload, (size_t)payload_len);
    memcpy(buf + 12 + payload_len, "\x00\x00", 2);

    int rc = send_all(fd, buf, 4 + total_len);
    free(buf);
    return rc;
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

/*
 * Read the RCON password from a file into out (NUL-terminated, at most
 * out_sz-1 bytes). A single trailing newline / CR is stripped so a file
 * written by `... > server.passwd` (base64 leaves a trailing '\n') matches
 * what `$(cat file)` would have produced. Returns 0 on success, -1 on error.
 *
 * Reading from a file keeps the secret out of argv (/proc/<pid>/cmdline, which
 * is world-readable) and out of the environment (/proc/<pid>/environ) entirely.
 */
static int read_password_file(const char *path, char *out, size_t out_sz)
{
    /* O_CLOEXEC so the descriptor can't leak into any process we might spawn. */
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        fprintf(stderr, "rcon: cannot open password file '%s': %s\n",
                path, strerror(errno));
        return -1;
    }

    size_t total = 0;
    while (total < out_sz - 1) {
        ssize_t n = read(fd, out + total, out_sz - 1 - total);
        if (n < 0) {
            if (errno == EINTR) continue;
            fprintf(stderr, "rcon: error reading password file '%s': %s\n",
                    path, strerror(errno));
            explicit_bzero(out, out_sz);
            close(fd);
            return -1;
        }
        if (n == 0) break;  /* EOF */
        total += (size_t)n;
    }
    close(fd);

    out[total] = '\0';
    /* Strip a single trailing line ending (\n, \r, or \r\n). */
    while (total > 0 && (out[total - 1] == '\n' || out[total - 1] == '\r'))
        out[--total] = '\0';

    if (total == 0) {
        fprintf(stderr, "rcon: password file '%s' is empty\n", path);
        return -1;
    }
    return 0;
}

static void usage(const char *prog)
{
    fprintf(stderr,
        "Usage: %s [--password-file <path>] <host> <port> [<password>] [command ...]\n"
        "\n"
        "  -f, --password-file <path>  Read the RCON password from <path> instead of\n"
        "                              the command line. Preferred: keeps the secret\n"
        "                              out of argv (/proc/<pid>/cmdline) and the\n"
        "                              process environment.\n"
        "\n"
        "  If --password-file is omitted, <password> must be given positionally\n"
        "  (legacy form). The positional password is scrubbed from argv at startup,\n"
        "  but is briefly visible to other local processes before that happens.\n",
        prog);
}

/* ── Main ────────────────────────────────────────────────────────────────── */

int main(int argc, char *argv[])
{
    /* Ignore SIGPIPE so that write() returns -1/EPIPE when the server closes
     * the connection mid-send, rather than killing the process silently. */
    signal(SIGPIPE, SIG_IGN);

    /* Parse any leading options. Option parsing stops at the first positional
     * argument, so command words (which follow host/port) are never mistaken
     * for options even if they begin with '-'. */
    const char *pw_file = NULL;
    int i = 1;
    while (i < argc && argv[i][0] == '-' && argv[i][1] != '\0') {
        if (strcmp(argv[i], "--password-file") == 0 || strcmp(argv[i], "-f") == 0) {
            if (i + 1 >= argc) { usage(argv[0]); return 1; }
            pw_file = argv[i + 1];
            i += 2;
        } else if (strcmp(argv[i], "--") == 0) {
            i++;
            break;
        } else {
            fprintf(stderr, "rcon: unknown option '%s'\n\n", argv[i]);
            usage(argv[0]);
            return 1;
        }
    }

    /* Remaining positionals: <host> <port> [<password>] [command ...].
     * The positional password is required only when --password-file is absent. */
    int need = pw_file ? 2 : 3;
    if (argc - i < need) {
        usage(argv[0]);
        return 1;
    }

    const char *host = argv[i];
    const char *port = argv[i + 1];

    char password[MAX_PAYLOAD + 1];
    int cmd_start;  /* index of the first command word; >= argc means none */

    if (pw_file) {
        if (read_password_file(pw_file, password, sizeof(password)) < 0)
            return 1;
        cmd_start = i + 2;
    } else {
        /* Legacy positional password. Copy it out, then overwrite the argv slot
         * with '*' so the plaintext doesn't remain visible in /proc/<pid>/cmdline
         * or `ps`. There's still a race before this runs — prefer --password-file. */
        char *pw_arg = argv[i + 2];
        size_t pw_len = strlen(pw_arg);
        if (pw_len > MAX_PAYLOAD) pw_len = MAX_PAYLOAD;
        memcpy(password, pw_arg, pw_len);
        password[pw_len] = '\0';
        memset(pw_arg, '*', strlen(pw_arg));
        cmd_start = i + 3;
    }

    int fd = rcon_connect(host, port);
    if (fd < 0) {
        explicit_bzero(password, sizeof(password));
        return 1;
    }

    int auth_result = rcon_auth(fd, password);

    /* Zero the password buffer as soon as authentication is done so it doesn't
     * linger in stack memory for the rest of the process lifetime. */
    explicit_bzero(password, sizeof(password));

    if (auth_result < 0) {
        close(fd);
        return 1;
    }

    int ret;
    if (cmd_start < argc) {
        char *cmd = build_command(argc, argv, cmd_start);
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
