/* A descriptor that is not open answers POLLNVAL at once, as POSIX has it,
   even with no timeout and nothing else ready. */
#include <poll.h>

#include "check.h"

int main(void) {
    int ends[2];
    CHECK(pipe(ends) == 0, "pipe");
    CHECK(close(ends[1]) == 0 && close(ends[0]) == 0, "close");
    struct pollfd fds[1] = {{.fd = ends[0], .events = POLLIN}};
    int ready = poll(fds, 1, -1);
    CHECK(ready == 1, "poll answered %d", ready);
    CHECK(fds[0].revents == POLLNVAL, "revents %#x", fds[0].revents);
    printf("poll_closed: a closed descriptor answered POLLNVAL\n");
    return 0;
}
