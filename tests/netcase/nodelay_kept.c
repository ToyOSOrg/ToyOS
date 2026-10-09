/* A TCP_NODELAY set on a stream socket before connect is kept and holds for
   the connection, and one cleared stays cleared. A listener's set is refused:
   netstack has no option for one to hand the connections it accepts, where a
   host keeps it and they begin with it. argv: the address and the port of a
   host that accepts and holds each connection. */
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>

static int wrong;

static void said(const char *what, long got, long want) {
    printf("%s: %ld%s\n", what, got, got == want ? "" : "  <-- WRONG");
    if (got != want) wrong++;
}

/* Whether TCP_NODELAY is set on `fd`: 1 or 0, or -1 where the read fails. */
static int nodelay(int fd) {
    int value = -7;
    socklen_t len = sizeof value;
    if (getsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &value, &len) != 0) return -1;
    return value != 0;
}

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    struct sockaddr_in peer;
    int on = 1, off = 0;

    memset(&peer, 0, sizeof peer);
    peer.sin_family = AF_INET;
    peer.sin_port = htons((uint16_t)atoi(argv[2]));
    said("the peer's address is one", inet_pton(AF_INET, argv[1], &peer.sin_addr), 1);

    int t = socket(AF_INET, SOCK_STREAM, 0);
    said("a fresh stream socket's TCP_NODELAY", nodelay(t), 0);
    said("set before connect", setsockopt(t, IPPROTO_TCP, TCP_NODELAY, &on, sizeof on), 0);
    said("read before connect", nodelay(t), 1);
    said("connect", connect(t, (struct sockaddr *)&peer, sizeof peer), 0);
    said("read after connect", nodelay(t), 1);
    said("cleared on the connection", setsockopt(t, IPPROTO_TCP, TCP_NODELAY, &off, sizeof off), 0);
    said("read after the clear", nodelay(t), 0);
    said("set on the connection", setsockopt(t, IPPROTO_TCP, TCP_NODELAY, &on, sizeof on), 0);
    said("read after the set", nodelay(t), 1);

    int u = socket(AF_INET, SOCK_STREAM, 0);
    said("set on a second socket", setsockopt(u, IPPROTO_TCP, TCP_NODELAY, &on, sizeof on), 0);
    said("and cleared before connect", setsockopt(u, IPPROTO_TCP, TCP_NODELAY, &off, sizeof off), 0);
    said("connect", connect(u, (struct sockaddr *)&peer, sizeof peer), 0);
    said("read after connect", nodelay(u), 0);

    struct sockaddr_in any;
    memset(&any, 0, sizeof any);
    any.sin_family = AF_INET;
    int l = socket(AF_INET, SOCK_STREAM, 0);
    said("bind a third socket", bind(l, (struct sockaddr *)&any, sizeof any), 0);
    said("listen on it", listen(l, 1), 0);
    errno = 0;
    said("set on the listener", setsockopt(l, IPPROTO_TCP, TCP_NODELAY, &on, sizeof on), -1);
    said("which is refused as not connected", errno == ENOTCONN, 1);
    said("read from the listener", nodelay(l), 0);

    if (wrong) {
        printf("nodelay_kept: %d wrong\n", wrong);
        return 1;
    }
    printf("nodelay_kept: ok\n");
    return 0;
}
