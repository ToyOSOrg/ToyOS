/* At most 255 children wait unwaited: the 256th spawn is refused EAGAIN,
   and beside the 255 a poll of 255 descriptors answers. */
#include <poll.h>

#include "check.h"

#define SELF "/system/bin/test_c_spawn_children_cap"
#define MAX 255

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
    CHECK(refused == EAGAIN, "the 256th unwaited child's spawn answered %d", refused);

    struct pollfd fds[MAX];
    for (int i = 0; i < MAX; i++)
        fds[i] = (struct pollfd){.fd = held[i], .events = POLLOUT};
    int ready = poll(fds, MAX, 0);
    CHECK(ready == MAX, "a poll of %d writable pipes beside %d children answered %d", MAX, MAX, ready);

    for (int i = 0; i < MAX; i++) {
        close(held[i]);
        int status = waited(pids[i]);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "child %d's status %#x", i, status);
    }
    printf("spawn_children_cap: the 256th unwaited child was refused EAGAIN, and a poll of 255 answered beside 255\n");
    return 0;
}
