/* At most 254 children wait unwaited, which libc's own thread watches
   beside its control event and the notice: the 255th spawn is refused EAGAIN,
   and beside the 254 a poll of 255 descriptors, a poller's handles but the
   wake event's, answers. */
#include <poll.h>

#include "check.h"

#define SELF "/system/bin/test_c_spawn_children_cap"
#define MAX 254
#define POLLED 255

static pid_t pids[MAX];
static int held[MAX];

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "held") == 0) {
        char byte;
        while (read(0, &byte, 1) > 0) {
        }
        return 0;
    }
    for (int i = 0; i < MAX; i++) {
        int ends[2];
        CHECK(pipe(ends) == 0, "pipe %d", i);
        posix_spawn_file_actions_t actions;
        CHECK(posix_spawn_file_actions_init(&actions) == 0, "refused");
        CHECK(posix_spawn_file_actions_adddup2(&actions, ends[0], 0) == 0, "refused");
        pids[i] = spawn_role(SELF, "held", "", &actions, NULL);
        posix_spawn_file_actions_destroy(&actions);
        close(ends[0]);
        held[i] = ends[1];
    }
    char *argv_[] = {SELF, "held", NULL};
    pid_t refused_pid;
    int refused = posix_spawn(&refused_pid, SELF, NULL, NULL, argv_, environ);
    CHECK(refused == EAGAIN, "the 255th unwaited child's spawn answered %d", refused);

    struct pollfd fds[POLLED];
    for (int i = 0; i < POLLED; i++)
        fds[i] = (struct pollfd){.fd = held[i % MAX], .events = POLLOUT};
    int ready = poll(fds, POLLED, 0);
    CHECK(ready == POLLED, "a poll of %d writable pipes beside %d children answered %d", POLLED, MAX, ready);
    struct pollfd more[POLLED + 1];
    for (int i = 0; i <= POLLED; i++)
        more[i] = (struct pollfd){.fd = held[0], .events = POLLOUT};
    errno = 0;
    CHECK(poll(more, POLLED + 1, 0) == -1 && errno == EINVAL, "a poll of %d answered", POLLED + 1);

    for (int i = 0; i < MAX; i++) {
        close(held[i]);
        int status = waited(pids[i]);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "child %d's status %#x", i, status);
    }
    printf("spawn_children_cap: the 255th unwaited child was refused EAGAIN, and a poll of 255 answered beside 254, and of 256 EINVAL\n");
    return 0;
}
