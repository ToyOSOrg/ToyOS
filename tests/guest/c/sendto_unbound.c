/* A datagram socket never bound is bound by its first send, to a port the
   stack chooses, as POSIX has it: the usual C broadcast sequence, socket,
   setsockopt, sendto, and a connected socket's send. argv: the address and
   the port of a host that answers each datagram with itself. */
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>

static int wrong;

static void said(const char *what, long got, long want) {
    printf("%s: %ld%s\n", what, got, got == want ? "" : "  <-- WRONG");
    if (got != want) wrong++;
}

/* The port `fd` is bound to, in host order. */
static unsigned bound_port(int fd) {
    struct sockaddr_in name;
    socklen_t len = sizeof name;
    memset(&name, 0, sizeof name);
    if (getsockname(fd, (struct sockaddr *)&name, &len) != 0) return 0;
    return ntohs(name.sin_port);
}

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    struct sockaddr_in peer, from, all;
    socklen_t fromlen = sizeof from;
    char buf[16];
    int on = 1, held = 0;
    socklen_t heldlen = sizeof held;

    memset(&peer, 0, sizeof peer);
    peer.sin_family = AF_INET;
    peer.sin_port = htons((uint16_t)atoi(argv[2]));
    said("the peer's address is one", inet_pton(AF_INET, argv[1], &peer.sin_addr), 1);

    int s = socket(AF_INET, SOCK_DGRAM, 0);
    said("SO_BROADCAST set on a socket never bound", setsockopt(s, SOL_SOCKET, SO_BROADCAST, &on, sizeof on), 0);
    said("sendto from it", sendto(s, "ping", 4, 0, (struct sockaddr *)&peer, sizeof peer), 4);
    said("the send bound it to a port", bound_port(s) != 0, 1);

    memset(&from, 0, sizeof from);
    memset(buf, 0, sizeof buf);
    said("the answer came back to that port", recvfrom(s, buf, sizeof buf, 0, (struct sockaddr *)&from, &fromlen), 4);
    said("and is what was sent", memcmp(buf, "ping", 4) == 0, 1);
    said("from the peer's address", from.sin_addr.s_addr == peer.sin_addr.s_addr, 1);
    said("and the peer's port", from.sin_port == peer.sin_port, 1);

    said("the option is read back", getsockopt(s, SOL_SOCKET, SO_BROADCAST, &held, &heldlen), 0);
    said("and is still set", held != 0, 1);
    memset(&all, 0, sizeof all);
    all.sin_family = AF_INET;
    all.sin_port = htons(9);
    all.sin_addr.s_addr = htonl(INADDR_BROADCAST);
    said("sendto the limited broadcast address", sendto(s, "x", 1, 0, (struct sockaddr *)&all, sizeof all), 1);

    int c = socket(AF_INET, SOCK_DGRAM, 0);
    said("connect on a second socket never bound", connect(c, (struct sockaddr *)&peer, sizeof peer), 0);
    said("send from it", send(c, "pong", 4, 0), 4);
    memset(buf, 0, sizeof buf);
    said("its answer", recv(c, buf, sizeof buf, 0), 4);
    said("is what it sent", memcmp(buf, "pong", 4) == 0, 1);
    said("the two sockets hold two ports", bound_port(c) != 0 && bound_port(c) != bound_port(s), 1);

    if (wrong) {
        printf("sendto_unbound: %d wrong\n", wrong);
        return 1;
    }
    printf("sendto_unbound: ok\n");
    return 0;
}
