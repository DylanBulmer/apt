/*
 * mcrcon — minimal RCON client for Minecraft servers
 *
 * Protocol (all integers little-endian):
 *   [length:i32][id:i32][type:i32][payload:bytes][pad:\x00\x00]
 *   length = 4 + 4 + len(payload) + 2
 *
 * Auth:    type 3, payload = password; response id == -1 means denied
 * Command: type 2, payload = command;  response payload = server output
 */

#include <arpa/inet.h>
#include <errno.h>
#include <netdb.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

#define RCON_AUTH    3
#define RCON_EXEC    2
#define MAX_PAYLOAD  1446  /* Minecraft's documented RCON payload limit */
#define IO_TIMEOUT_S 10    /* seconds before read/write gives up */

/* Portable little-endian helpers — work on both LE and BE hosts */
static uint32_t u32_to_le(uint32_t v) {
    uint8_t b[4] = { v & 0xFF, (v >> 8) & 0xFF, (v >> 16) & 0xFF, v >> 24 };
    uint32_t r = 0;
    memcpy(&r, b, 4);
    return r;
}
static uint32_t le_to_u32(uint32_t v) { return u32_to_le(v); } /* symmetric */

/* ── I/O helpers ─────────────────────────────────────────────────────────── */

static int send_all(int fd, const void *buf, size_t n)
{
    const char *p = buf;
    while (n) {
        ssize_t w = write(fd, p, n);
        if (w <= 0) return -1;
        p += w; n -= (size_t)w;
    }
    return 0;
}

static int recv_all(int fd, void *buf, size_t n)
{
    char *p = buf;
    while (n) {
        ssize_t r = read(fd, p, n);
        if (r <= 0) return -1;
        p += r; n -= (size_t)r;
    }
    return 0;
}

/* ── RCON packet I/O ─────────────────────────────────────────────────────── */

static int pkt_send(int fd, int32_t id, int32_t type, const char *payload)
{
    size_t raw_len = strlen(payload);
    if (raw_len > MAX_PAYLOAD) {
        fprintf(stderr, "mcrcon: payload too large (%zu > %d bytes)\n",
                raw_len, MAX_PAYLOAD);
        return -1;
    }

    int32_t payload_len = (int32_t)raw_len;
    int32_t length      = 4 + 4 + payload_len + 2;
    int32_t le_len      = (int32_t)u32_to_le((uint32_t)length);
    int32_t le_id       = (int32_t)u32_to_le((uint32_t)id);
    int32_t le_type     = (int32_t)u32_to_le((uint32_t)type);

    if (send_all(fd, &le_len,  4) < 0) return -1;
    if (send_all(fd, &le_id,   4) < 0) return -1;
    if (send_all(fd, &le_type, 4) < 0) return -1;
    if (send_all(fd, payload, (size_t)payload_len) < 0) return -1;
    if (send_all(fd, "\x00\x00", 2) < 0) return -1;
    return 0;
}

/*
 * Receive one packet. On success, *out_payload is heap-allocated (caller frees).
 * Returns 0 on success, -1 on error.
 */
static int pkt_recv(int fd, int32_t *out_id, int32_t *out_type, char **out_payload)
{
    int32_t le_len, le_id, le_type;

    if (recv_all(fd, &le_len, 4) < 0) return -1;

    int32_t length = (int32_t)le_to_u32((uint32_t)le_len);
    if (length < 10 || length > MAX_PAYLOAD + 10) {
        fprintf(stderr, "mcrcon: invalid packet length %d\n", length);
        return -1;
    }

    if (recv_all(fd, &le_id,   4) < 0) return -1;
    if (recv_all(fd, &le_type, 4) < 0) return -1;

    *out_id   = (int32_t)le_to_u32((uint32_t)le_id);
    *out_type = (int32_t)le_to_u32((uint32_t)le_type);

    int32_t data_len = length - 10;
    char *payload = malloc((size_t)data_len + 1);
    if (!payload) return -1;

    if (data_len > 0 && recv_all(fd, payload, (size_t)data_len) < 0) {
        free(payload);
        return -1;
    }
    payload[data_len] = '\0';

    char pad[2];
    if (recv_all(fd, pad, 2) < 0) { free(payload); return -1; }

    *out_payload = payload;
    return 0;
}

/* ── Connection helpers ───────────────────────────────────────────────────── */

static int rcon_connect(const char *host, const char *port)
{
    struct addrinfo hints = {0}, *res;
    hints.ai_family   = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;

    int r = getaddrinfo(host, port, &hints, &res);
    if (r != 0) {
        fprintf(stderr, "mcrcon: %s: %s\n", host, gai_strerror(r));
        return -1;
    }

    int fd = socket(res->ai_family, res->ai_socktype, res->ai_protocol);
    if (fd < 0) { freeaddrinfo(res); return -1; }

    /* Apply I/O timeouts so a hung server can't block us forever */
    struct timeval tv = { .tv_sec = IO_TIMEOUT_S, .tv_usec = 0 };
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));

    if (connect(fd, res->ai_addr, res->ai_addrlen) < 0) {
        fprintf(stderr, "mcrcon: connect to %s:%s: %s\n", host, port, strerror(errno));
        close(fd);
        freeaddrinfo(res);
        return -1;
    }

    freeaddrinfo(res);
    return fd;
}

static int rcon_auth(int fd, const char *password)
{
    if (pkt_send(fd, 1, RCON_AUTH, password) < 0) return -1;

    int32_t id, type;
    char *payload;
    if (pkt_recv(fd, &id, &type, &payload) < 0) return -1;
    free(payload);

    if (id == -1) {
        fprintf(stderr, "mcrcon: authentication failed — check password\n");
        return -1;
    }
    return 0;
}

/* Send a command and return the response (caller frees). NULL on error. */
static char *rcon_exec(int fd, const char *cmd)
{
    if (pkt_send(fd, 2, RCON_EXEC, cmd) < 0) return NULL;

    int32_t id, type;
    char *payload;
    if (pkt_recv(fd, &id, &type, &payload) < 0) return NULL;
    return payload;
}

/* ── Main ────────────────────────────────────────────────────────────────── */

int main(int argc, char *argv[])
{
    if (argc < 4) {
        fprintf(stderr, "Usage: %s <host> <port> <password> [command ...]\n", argv[0]);
        return 1;
    }

    const char *host = argv[1];
    const char *port = argv[2];

    /* Copy password then scrub argv so it doesn't linger in /proc/<pid>/cmdline */
    char password[MAX_PAYLOAD + 1];
    size_t pw_len = strlen(argv[3]);
    if (pw_len > MAX_PAYLOAD) pw_len = MAX_PAYLOAD;
    memcpy(password, argv[3], pw_len);
    password[pw_len] = '\0';
    memset(argv[3], '*', strlen(argv[3]));

    int fd = rcon_connect(host, port);
    if (fd < 0) return 1;

    if (rcon_auth(fd, password) < 0) {
        close(fd);
        return 1;
    }

    int ret = 0;

    if (argc > 4) {
        /* Measure total command length and enforce the protocol limit */
        size_t total = 0;
        for (int i = 4; i < argc; i++) {
            size_t arg_len = strlen(argv[i]);
            if (total + arg_len + 1 > MAX_PAYLOAD) {
                fprintf(stderr, "mcrcon: command too long (> %d bytes)\n", MAX_PAYLOAD);
                close(fd);
                return 1;
            }
            total += arg_len + 1; /* +1 for the joining space */
        }

        char *cmd = malloc(total + 1);
        if (!cmd) { close(fd); return 1; }

        /* Build the command string with pointer tracking to avoid O(n²) strcat */
        char *p = cmd;
        for (int i = 4; i < argc; i++) {
            if (i > 4) *p++ = ' ';
            size_t len = strlen(argv[i]);
            memcpy(p, argv[i], len);
            p += len;
        }
        *p = '\0';

        char *resp = rcon_exec(fd, cmd);
        free(cmd);
        if (!resp) { ret = 1; goto done; }
        if (*resp) puts(resp);
        free(resp);

    } else {
        /* Interactive mode */
        fprintf(stderr, "Connected to %s:%s — type a command, or Ctrl+D to exit.\n",
                host, port);

        char line[MAX_PAYLOAD + 1];
        for (;;) {
            printf("rcon> ");
            fflush(stdout);

            if (!fgets(line, (int)sizeof(line), stdin)) {
                putchar('\n');
                break;
            }

            /* Strip trailing newline */
            size_t len = strlen(line);
            if (len && line[len - 1] == '\n') line[--len] = '\0';
            if (!len) continue;
            if (strcmp(line, "exit") == 0 || strcmp(line, "quit") == 0) break;

            char *resp = rcon_exec(fd, line);
            if (!resp) {
                fprintf(stderr, "mcrcon: connection lost\n");
                ret = 1;
                break;
            }
            if (*resp) puts(resp);
            free(resp);
        }
    }

done:
    close(fd);
    return ret;
}
