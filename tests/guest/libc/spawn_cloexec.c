/* A descriptor the parent marked FD_CLOEXEC, a 0 that addopen opened
   O_CLOEXEC, and a 1 that adddup2 made and addclose closed are each absent in
   the child. Each child asks about one: a child that holds it exits 0, and
   one that does not is answered EBADF and exits 2. */
#include <errno.h>
#include <fcntl.h>
#include <sys/stat.h>

#include "check.h"

#define SELF "/system/bin/test_c_spawn_cloexec"

/* The child's verdict on descriptor `fd`: 0 held, 2 not, and 3 for any
   other answer. */
static int holds(int fd) {
    struct stat st;
    if (fstat(fd, &st) == 0)
        return 0;
    return errno == EBADF ? 2 : 3;
}

/* How a child asking about `fd`, its descriptors as `actions` leave them,
   ended: 1 for held, 0 for absent. */
static int child_holds(const char *fd, const posix_spawn_file_actions_t *actions) {
    int status = waited(spawn_role(SELF, "holds", fd, actions, NULL));
    CHECK(WIFEXITED(status) && (WEXITSTATUS(status) == 0 || WEXITSTATUS(status) == 2),
          "the child asking about %s ended %#x", fd, status);
    return WEXITSTATUS(status) == 0;
}

/* A process started without a stdout starts children of its own: the
   descriptor it lacks is none its children are given, and nothing ends it. */
static int spawns_without_stdout(void) {
    char *argv[] = {SELF, "holds", "0", NULL};
    pid_t pid;
    if (posix_spawn(&pid, SELF, NULL, NULL, argv, environ) != 0)
        return 3;
    int status;
    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status) || WEXITSTATUS(status) != 0)
        return 4;
    return system("exit 0") == 0 ? 0 : 5;
}

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "holds") == 0)
        return holds(atoi(argv[2]));
    if (argc >= 2 && strcmp(argv[1], "spawner") == 0)
        return spawns_without_stdout();

    CHECK(child_holds("1", NULL), "a child spawned with no actions lacks stdout");

    int was = fcntl(1, F_GETFD);
    CHECK(fcntl(1, F_SETFD, FD_CLOEXEC) == 0 && fcntl(1, F_GETFD) == FD_CLOEXEC, "F_SETFD");
    int stdout_held = child_holds("1", NULL);
    CHECK(fcntl(1, F_SETFD, was) == 0, "F_SETFD back");
    CHECK(!stdout_held, "the child holds the stdout its parent marked FD_CLOEXEC");

    int fd = open("/tmp/spawn_cloexec", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0, "create /tmp/spawn_cloexec");
    close(fd);
    posix_spawn_file_actions_t actions;
    CHECK(posix_spawn_file_actions_init(&actions) == 0, "refused");
    CHECK(posix_spawn_file_actions_addopen(&actions, 0, "/tmp/spawn_cloexec", O_RDONLY | O_CLOEXEC, 0) == 0, "refused");
    CHECK(!child_holds("0", &actions), "the child holds a 0 addopen opened O_CLOEXEC");
    posix_spawn_file_actions_destroy(&actions);

    CHECK(posix_spawn_file_actions_init(&actions) == 0, "refused");
    CHECK(posix_spawn_file_actions_addopen(&actions, 0, "/tmp/spawn_cloexec", O_RDONLY, 0) == 0, "refused");
    CHECK(child_holds("0", &actions), "the child lacks a 0 addopen opened");
    posix_spawn_file_actions_destroy(&actions);

    int ends[2];
    CHECK(pipe(ends) == 0, "pipe");
    CHECK(posix_spawn_file_actions_init(&actions) == 0, "refused");
    CHECK(posix_spawn_file_actions_adddup2(&actions, ends[1], 5) == 0, "refused");
    CHECK(child_holds("5", &actions), "the child lacks a 5 adddup2 made");
    CHECK(posix_spawn_file_actions_addclose(&actions, 5) == 0, "refused");
    CHECK(!child_holds("5", &actions), "the child holds a 5 adddup2 made and addclose closed");
    posix_spawn_file_actions_destroy(&actions);

    CHECK(posix_spawn_file_actions_init(&actions) == 0, "refused");
    CHECK(posix_spawn_file_actions_addclose(&actions, 1) == 0, "refused");
    CHECK(!child_holds("1", &actions), "the child holds a 1 addclose closed");
    posix_spawn_file_actions_destroy(&actions);
    posix_spawn_file_actions_init(&actions);
    posix_spawn_file_actions_addclose(&actions, 1);
    int status = waited(spawn_role(SELF, "spawner", "", &actions, NULL));
    posix_spawn_file_actions_destroy(&actions);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "a child without a stdout, spawning, ended %#x", status);
    printf("spawn_cloexec: FD_CLOEXEC, O_CLOEXEC and addclose each kept a descriptor from the child, and a child without one spawned its own\n");
    return 0;
}
