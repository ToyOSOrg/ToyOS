/* An IPv4 address is in network byte order wherever a C program hands libc
   one or is handed one: connect reaches the same host for an address built
   with inet_pton, with htonl, with inet_addr and by getaddrinfo, and
   getpeername answers it. argv: the address, as four decimal octets, and the
   port of a host that accepts and holds each connection. */
#include <arpa/inet.h>
#include <netdb.h>
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

/* connect a fresh stream socket to `addr` at `port`; the socket, or -1. */
static int dial(struct in_addr addr, in_port_t port) {
    struct sockaddr_in to;
    memset(&to, 0, sizeof to);
    to.sin_family = AF_INET;
    to.sin_port = port;
    to.sin_addr = addr;
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    return connect(fd, (struct sockaddr *)&to, sizeof to) == 0 ? fd : -1;
}

static void text_of(const char *what, in_addr_t host_order, const char *want) {
    struct in_addr a;
    char text[INET_ADDRSTRLEN];
    a.s_addr = htonl(host_order);
    const char *got = inet_ntop(AF_INET, &a, text, sizeof text);
    printf("%s: %s%s\n", what, got ? got : "(null)", got && strcmp(got, want) == 0 ? "" : "  <-- WRONG");
    if (!got || strcmp(got, want) != 0) wrong++;
}

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    in_port_t port = htons((uint16_t)atoi(argv[2]));
    unsigned char octets[4], in_memory[4];
    struct in_addr by_pton, by_htonl, by_inet_addr;
    char text[INET_ADDRSTRLEN];

    /* The octets, read from the text by nothing of the network's. */
    char *at = argv[1];
    for (int i = 0; i < 4; i++) {
        octets[i] = (unsigned char)strtoul(at, &at, 10);
        at++;
    }
    in_addr_t host_order = (in_addr_t)octets[0] << 24 | (in_addr_t)octets[1] << 16 | (in_addr_t)octets[2] << 8 | octets[3];

    said("inet_pton reads the address", inet_pton(AF_INET, argv[1], &by_pton), 1);
    memcpy(in_memory, &by_pton, 4);
    said("into its octets in memory order", memcmp(in_memory, octets, 4) == 0, 1);
    by_htonl.s_addr = htonl(host_order);
    said("htonl builds the same four bytes", memcmp(&by_htonl, octets, 4) == 0, 1);
    by_inet_addr.s_addr = inet_addr(argv[1]);
    said("and so does inet_addr", memcmp(&by_inet_addr, octets, 4) == 0, 1);
    said("ntohl gives the number back", ntohl(by_pton.s_addr) == host_order, 1);

    int a = dial(by_pton, port);
    said("connect to inet_pton's address", a >= 0, 1);
    said("connect to htonl's", dial(by_htonl, port) >= 0, 1);
    said("connect to inet_addr's", dial(by_inet_addr, port) >= 0, 1);

    struct sockaddr_in name;
    socklen_t len = sizeof name;
    memset(&name, 0, sizeof name);
    said("getpeername", getpeername(a, (struct sockaddr *)&name, &len), 0);
    said("answers the address", memcmp(&name.sin_addr, octets, 4) == 0, 1);
    said("and the port", name.sin_port == port, 1);
    const char *shown = inet_ntop(AF_INET, &name.sin_addr, text, sizeof text);
    said("which inet_ntop writes as it was given", shown && strcmp(shown, argv[1]) == 0, 1);

    struct addrinfo hints, *found = NULL;
    memset(&hints, 0, sizeof hints);
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    said("getaddrinfo of the address", getaddrinfo(argv[1], NULL, &hints, &found), 0);
    if (found) {
        struct in_addr by_getaddrinfo = ((struct sockaddr_in *)found->ai_addr)->sin_addr;
        said("answers the same four bytes", memcmp(&by_getaddrinfo, octets, 4) == 0, 1);
        said("connect to getaddrinfo's", dial(by_getaddrinfo, port) >= 0, 1);
        freeaddrinfo(found);
    }

    text_of("INADDR_LOOPBACK", INADDR_LOOPBACK, "127.0.0.1");
    text_of("INADDR_BROADCAST", INADDR_BROADCAST, "255.255.255.255");
    text_of("INADDR_ANY", INADDR_ANY, "0.0.0.0");

    if (wrong) {
        printf("addr_order: %d wrong\n", wrong);
        return 1;
    }
    printf("addr_order: ok\n");
    return 0;
}
