/* A child's stdout on a pipe is read to its end through poll, and then the
   child is waited for: the end comes only once the child, the one holder of
   the write end the spawn handed it, has ended. */
#include <poll.h>

#include "check.h"

int main(void) {
    int ends[2];
    CHECK(pipe(ends) == 0, "pipe");
    posix_spawn_file_actions_t actions;
    CHECK(posix_spawn_file_actions_init(&actions) == 0, "refused");
    CHECK(posix_spawn_file_actions_adddup2(&actions, ends[1], 1) == 0, "refused");
    char *argv[] = {"echo", "through the pipe", NULL};
    pid_t pid;
    int refused = posix_spawn(&pid, "/system/bin/echo", &actions, NULL, argv, environ);
    CHECK(refused == 0, "posix_spawn echo answered %d", refused);
    posix_spawn_file_actions_destroy(&actions);
    close(ends[1]);

    char said[64];
    size_t got = 0;
    for (;;) {
        struct pollfd fd = {.fd = ends[0], .events = POLLIN};
        int ready = poll(&fd, 1, -1);
        CHECK(ready == 1, "poll answered %d", ready);
        ssize_t n = read(ends[0], said + got, sizeof said - 1 - got);
        CHECK(n >= 0, "read answered %zd", n);
        if (n == 0)
            break;
        got += (size_t)n;
    }
    said[got] = 0;
    CHECK(strcmp(said, "through the pipe\n") == 0, "the child said %s", said);
    int status = waited(pid);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "echo's status %#x", status);
    printf("spawn_pipe_eof: a piped stdout read to its end through poll, then waited for\n");
    return 0;
}
