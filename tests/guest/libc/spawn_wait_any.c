/* waitpid(-1) answers each child once, in the order they end, and then
   ECHILD; wait and wait4 answer as it does. Each child ends when its stdin
   closes, so the order is the test's and no clock decides it. */
#include <sys/resource.h>

#include "check.h"

#define SELF "/system/bin/test_c_spawn_wait_any"

#define CHILDREN 3

static int child(const char *code) {
    char byte;
    while (read(0, &byte, 1) > 0) {
    }
    return atoi(code);
}

/* Three held children, each with the write end of its stdin kept here. */
static void start(pid_t pids[CHILDREN], int held[CHILDREN]) {
    for (int i = 0; i < CHILDREN; i++) {
        int ends[2];
        CHECK(pipe(ends) == 0, "pipe");
        posix_spawn_file_actions_t actions;
        CHECK(posix_spawn_file_actions_init(&actions) == 0, "refused");
        CHECK(posix_spawn_file_actions_adddup2(&actions, ends[0], 0) == 0, "refused");
        char code[4];
        snprintf(code, sizeof code, "%d", 10 + i);
        pids[i] = spawn_role(SELF, "child", code, &actions, NULL);
        posix_spawn_file_actions_destroy(&actions);
        close(ends[0]);
        held[i] = ends[1];
    }
}

typedef pid_t (*Waiter)(int *status);

static pid_t by_waitpid(int *status) { return waitpid(-1, status, 0); }
static pid_t by_wait(int *status) { return wait(status); }
static pid_t by_wait4(int *status) {
    struct rusage usage;
    return wait4(-1, status, 0, &usage);
}

static void in_end_order(const char *name, Waiter waiter) {
    pid_t pids[CHILDREN];
    int held[CHILDREN];
    start(pids, held);
    const int order[CHILDREN] = {2, 0, 1};
    for (int k = 0; k < CHILDREN; k++) {
        int i = order[k];
        close(held[i]);
        int status = -1;
        pid_t got = waiter(&status);
        CHECK(got == pids[i], "%s answered %d, and child %d (pid %d) ended", name, got, i, pids[i]);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 10 + i, "%s: child %d's status %#x", name, i, status);
    }
    int status;
    errno = 0;
    CHECK(waiter(&status) == -1 && errno == ECHILD, "%s with every child waited", name);
}

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "child") == 0)
        return child(argv[2]);
    in_end_order("waitpid(-1)", by_waitpid);
    in_end_order("wait", by_wait);
    in_end_order("wait4", by_wait4);
    printf("spawn_wait_any: waitpid(-1), wait and wait4 answered three children in the order they ended, then ECHILD\n");
    return 0;
}
