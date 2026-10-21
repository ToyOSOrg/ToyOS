/* A negative descriptor in poll's array is passed over, as POSIX has it, and
   the readable pipe beside it answers. */
#include <poll.h>

#include "check.h"

int main(void) {
    int ends[2];
    CHECK(pipe(ends) == 0, "pipe");
    CHECK(write(ends[1], "x", 1) == 1, "write");
    struct pollfd fds[2] = {{.fd = -1, .events = POLLIN}, {.fd = ends[0], .events = POLLIN}};
    int ready = poll(fds, 2, -1);
    CHECK(ready == 1, "poll answered %d", ready);
    CHECK(fds[0].revents == 0 && (fds[1].revents & POLLIN), "revents %#x and %#x", fds[0].revents, fds[1].revents);
    printf("poll_negative: a negative descriptor was passed over and the pipe beside it answered\n");
    return 0;
}
