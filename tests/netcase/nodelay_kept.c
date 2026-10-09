/* A TCP_NODELAY set on a stream socket before connect is kept and holds for
   the connection, and one cleared stays cleared. One set before bind is the
   listener's, and each connection it accepts begins with it. A listener's set
   after bind is refused: netstack has no option for a listener, where a host
   takes the set and gives it to the connections that begin afterwards. argv:
   the address and the port of a host that accepts and holds each connection,
   then the two ports to listen on, which that host dials once WAITING is
   said. */
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>

#define WAITING "nodelay_kept: both listeners wait for a peer"

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
    if (argc != 5) return 2;
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
    int held = socket(AF_INET, SOCK_STREAM, 0);
    said("set on a third socket before bind", setsockopt(held, IPPROTO_TCP, TCP_NODELAY, &on, sizeof on), 0);
    any.sin_port = htons((uint16_t)atoi(argv[3]));
    said("bind it", bind(held, (struct sockaddr *)&any, sizeof any), 0);
    said("listen on it", listen(held, 1), 0);
    said("read from that listener", nodelay(held), 1);
    int l = socket(AF_INET, SOCK_STREAM, 0);
    any.sin_port = htons((uint16_t)atoi(argv[4]));
    said("bind a fourth socket", bind(l, (struct sockaddr *)&any, sizeof any), 0);
    said("listen on it", listen(l, 1), 0);

    printf("%s\n", WAITING);
    fflush(stdout);
    int with = accept(held, NULL, NULL);
    said("accept from the listener that held the option", with >= 0, 1);
    said("read from its connection", nodelay(with), 1);
    int without = accept(l, NULL, NULL);
    said("accept from the listener that held none", without >= 0, 1);
    said("read from its connection", nodelay(without), 0);

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
