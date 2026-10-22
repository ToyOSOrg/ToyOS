/* kill(-pgid, SIGKILL) ends that group's two children and not a third, and
   kill(pid, 0) answers 0 for a live child and ESRCH once it is waited. */
#include <signal.h>

#include "check.h"

#define SELF "/system/bin/test_c_spawn_kill_group"

/* A child held until `*held` closes, in `group`: 0 for a group of its own,
   -1 for this process's. */
static pid_t start(pid_t group, int *held) {
    int ends[2];
    CHECK(pipe(ends) == 0, "pipe");
    posix_spawn_file_actions_t actions;
    CHECK(posix_spawn_file_actions_init(&actions) == 0, "refused");
    CHECK(posix_spawn_file_actions_adddup2(&actions, ends[0], 0) == 0, "refused");
    posix_spawnattr_t attr;
    CHECK(posix_spawnattr_init(&attr) == 0, "refused");
    if (group >= 0) {
        CHECK(posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETPGROUP) == 0, "refused");
        CHECK(posix_spawnattr_setpgroup(&attr, group) == 0, "refused");
    }
    pid_t pid = spawn_role(SELF, "held", "", &actions, &attr);
    posix_spawnattr_destroy(&attr);
    posix_spawn_file_actions_destroy(&actions);
    close(ends[0]);
    *held = ends[1];
    return pid;
}

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "held") == 0) {
        char byte;
        while (read(0, &byte, 1) > 0) {
        }
        return 0;
    }
    int held[3];
    pid_t leader = start(0, &held[0]);
    pid_t member = start(leader, &held[1]);
    pid_t other = start(-1, &held[2]);

    CHECK(kill(other, 0) == 0, "kill(%d, 0) of a live child", other);
    CHECK(kill(-leader, SIGKILL) == 0, "kill(-%d, SIGKILL)", leader);
    for (int i = 0; i < 2; i++) {
        pid_t pid = i == 0 ? leader : member;
        int status = waited(pid);
        CHECK(WIFSIGNALED(status) && WTERMSIG(status) == SIGKILL, "group child %d's status %#x", pid, status);
    }
    int status;
    CHECK(waitpid(other, &status, WNOHANG) == 0, "the child outside the group ended");
    CHECK(kill(other, 0) == 0, "kill(%d, 0) of a live child after the group's kill", other);
    close(held[2]);
    status = waited(other);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "the third child's status %#x", status);
    errno = 0;
    CHECK(kill(other, 0) == -1 && errno == ESRCH, "kill(%d, 0) of a child waited for", other);
    errno = 0;
    CHECK(kill(other, SIGUSR1) == -1 && errno == EINVAL, "kill with a signal no process takes");
    close(held[0]);
    close(held[1]);
    printf("spawn_kill_group: kill(-pgid) ended the group's two children and not the third; kill(pid, 0) probed\n");
    return 0;
}
