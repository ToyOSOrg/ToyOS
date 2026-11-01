/* With SIGCHLD ignored no ended child is kept: one child more than libc keeps
   unwaited starts, each waited for by a waitpid that blocks until it has ended
   and answers ECHILD, and then waitpid(-1) answers ECHILD. */
#include <signal.h>

#include "check.h"

#define SELF "/system/bin/test_c_signal_sigchld_ignored"

/* One past the 254 children libc keeps unwaited. */
#define CHILDREN 255

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "child") == 0)
        return 0;
    CHECK(signal(SIGCHLD, SIG_IGN) != SIG_ERR, "signal(SIGCHLD, SIG_IGN)");
    for (int i = 0; i < CHILDREN; i++) {
        pid_t pid = spawn_role(SELF, "child", "-", NULL, NULL);
        errno = 0;
        CHECK(waitpid(pid, NULL, 0) == -1 && errno == ECHILD, "waitpid of child %d under SIG_IGN", i);
    }
    errno = 0;
    CHECK(waitpid(-1, NULL, 0) == -1 && errno == ECHILD, "waitpid(-1) once every child ended");
    printf("signal_sigchld_ignored: %d children each ended unkept under SIG_IGN, and every wait answered ECHILD\n", CHILDREN);
    return 0;
}
