/* How libc tells a C client a stream ended: recv 0 at the peer's FIN after
   its own SHUT_WR, ECONNRESET on a reset mid-stream and after SHUT_WR, EPIPE
   for a send after SHUT_WR, and recv 0 after SHUT_RD. argv: the address and
   the port of the harness's peer, whose first byte read names how it ends the
   stream (`tests/toyos-rust-tests/src/stream_ends.rs`). No stream is closed:
   libc's close of a socket ends the program
   (`issues/libc-close-of-a-socket-ends-the-process.md`), and the job's exit
   lets each go. */
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>

/* What the peer sends before its reset of 'R'. */
#define AHEAD 65536

static int wrong;
static struct sockaddr_in peer;

static void said(const char *what, long got, long want) {
    printf("%s: %ld%s\n", what, got, got == want ? "" : "  <-- WRONG");
    if (got != want) wrong++;
}

/* A call that answered -1: the errno it set, named. */
static void refused(const char *what, long got, int err, int want) {
    printf("%s: %ld, errno %d%s\n", what, got, got < 0 ? err : 0, got < 0 && err == want ? "" : "  <-- WRONG");
    if (got >= 0 || err != want) wrong++;
}

/* A stream to the peer that has been told how to end it. */
static int dial(char how) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0 || connect(fd, (struct sockaddr *)&peer, sizeof peer) != 0 || send(fd, &how, 1, 0) != 1) {
        printf("stream_ends: the peer could not be dialled for '%c'\n", how);
        exit(1);
    }
    return fd;
}

/* Reads until `want` bytes or what ends them first: the bytes read. */
static long take(int fd, long want) {
    char buf[4096];
    long got = 0, r;
    while (got < want && (r = recv(fd, buf, sizeof buf < (size_t)(want - got) ? sizeof buf : (size_t)(want - got), 0)) > 0)
        got += r;
    return got;
}

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    char c;
    long r;
    memset(&peer, 0, sizeof peer);
    peer.sin_family = AF_INET;
    peer.sin_port = htons((uint16_t)atoi(argv[2]));
    said("the peer's address is one", inet_pton(AF_INET, argv[1], &peer.sin_addr), 1);

    int fd = dial('S');
    said("shut_first: the peer's answer", recv(fd, &c, 1, 0), 1);
    said("shut_first: shutdown(SHUT_WR)", shutdown(fd, SHUT_WR), 0);
    r = send(fd, "x", 1, 0);
    refused("shut_first: a send after it", r, errno, EPIPE);
    said("shut_first: the peer's four bytes after it", take(fd, 4), 4);
    said("shut_first: then the peer's FIN", recv(fd, &c, 1, 0), 0);

    fd = dial('R');
    said("reset_mid_stream: the bytes ahead of it", take(fd, AHEAD), AHEAD);
    said("reset_mid_stream: the answer", send(fd, "k", 1, 0), 1);
    r = recv(fd, &c, 1, 0);
    refused("reset_mid_stream: then", r, errno, ECONNRESET);

    fd = dial('D');
    said("reset_after_half_close: shutdown(SHUT_WR)", shutdown(fd, SHUT_WR), 0);
    r = recv(fd, &c, 1, 0);
    refused("reset_after_half_close: then", r, errno, ECONNRESET);

    fd = dial('B');
    said("shut_rd: shutdown(SHUT_RD) with the peer's three bytes on their way", shutdown(fd, SHUT_RD), 0);
    said("shut_rd: then recv", recv(fd, &c, 1, 0), 0);

    if (wrong) {
        printf("stream_ends: %d wrong\n", wrong);
        return 1;
    }
    printf("stream_ends: ok\n");
    return 0;
}
