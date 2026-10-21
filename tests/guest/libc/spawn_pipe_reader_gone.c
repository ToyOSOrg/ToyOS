/* A child writing to its stdout, a pipe whose one reader this process closed,
   is refused EPIPE: the spawn handed the child the write end and nothing kept
   a second read end. Its writes fill the pipe first if they come before the
   close, and then block until it. */
#include "check.h"

#define SELF "/system/bin/test_c_spawn_pipe_reader_gone"

static int writer(void) {
    static char block[4096];
    for (int i = 0; i < 1024; i++) {
        if (write(1, block, sizeof block) < 0)
            return errno == EPIPE ? 0 : 2;
    }
    return 3;
}

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "writer") == 0)
        return writer();
    int ends[2];
    CHECK(pipe(ends) == 0, "pipe");
    posix_spawn_file_actions_t actions;
    CHECK(posix_spawn_file_actions_init(&actions) == 0, "refused");
    CHECK(posix_spawn_file_actions_adddup2(&actions, ends[1], 1) == 0, "refused");
    pid_t pid = spawn_role(SELF, "writer", "", &actions, NULL);
    posix_spawn_file_actions_destroy(&actions);
    close(ends[1]);
    close(ends[0]);
    int status = waited(pid);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "the writer's status %#x: 2 for another error, 3 for every write taken",
          status);
    printf("spawn_pipe_reader_gone: a child's write to a pipe whose one reader closed was refused EPIPE\n");
    return 0;
}
