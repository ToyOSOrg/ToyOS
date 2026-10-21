/* A child's stdin is exactly the handle its file action named: given a pipe's
   write end, its read is refused, as the kernel refuses a read of a handle
   carrying no READ right, and nothing else stands in slot 0. */
#include "check.h"

#define SELF "/system/bin/test_c_spawn_stdio_named"

static int reader(void) {
    char byte;
    ssize_t n = read(0, &byte, 1);
    return n < 0 && errno == EACCES ? 0 : 4;
}

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "reader") == 0)
        return reader();
    int ends[2];
    CHECK(pipe(ends) == 0, "pipe");
    CHECK(write(ends[1], "x", 1) == 1, "write");
    posix_spawn_file_actions_t actions;
    CHECK(posix_spawn_file_actions_init(&actions) == 0, "refused");
    CHECK(posix_spawn_file_actions_adddup2(&actions, ends[1], 0) == 0, "refused");
    int status = waited(spawn_role(SELF, "reader", "", &actions, NULL));
    posix_spawn_file_actions_destroy(&actions);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "the reader's status %#x: 4 for a read that was not refused", status);
    printf("spawn_stdio_named: a child given a write end as its stdin was refused its read\n");
    return 0;
}
