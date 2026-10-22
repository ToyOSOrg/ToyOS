/* What every libc test here shares: a check that ends the test with exit 1
   and says which, and a child of the test itself, started by role. */
#ifndef CHECK_H
#define CHECK_H

#include <errno.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

#define CHECK(cond, ...)                                                     \
    do {                                                                     \
        if (!(cond)) {                                                       \
            printf("FAIL %s:%d: %s: ", __FILE__, __LINE__, #cond);           \
            printf(__VA_ARGS__);                                             \
            printf(" (errno %d)\n", errno);                                  \
            fflush(stdout);                                                  \
            exit(1);                                                         \
        }                                                                    \
    } while (0)

/* This test's own binary, `/system/bin/test_c_<name>` as the harness stages
   it, run as `role` with `arg`, its descriptors as `actions` leave them: the
   child's pid. */
static inline pid_t spawn_role(const char *self, const char *role, const char *arg,
                        const posix_spawn_file_actions_t *actions, const posix_spawnattr_t *attr) {
    char *argv[] = {(char *)self, (char *)role, (char *)arg, NULL};
    pid_t pid = -1;
    int refused = posix_spawn(&pid, self, actions, attr, argv, environ);
    CHECK(refused == 0, "posix_spawn %s %s answered %d", self, role, refused);
    return pid;
}

/* `pid`'s status, waited for. */
static inline int waited(pid_t pid) {
    int status = -1;
    pid_t got = waitpid(pid, &status, 0);
    CHECK(got == pid, "waitpid(%d) answered %d", pid, got);
    return status;
}

/* The next line on `fd`, read a byte at a time, which must be `expected`. */
static inline void expect_line(int fd, const char *expected) {
    char line[128];
    size_t n = 0;
    char byte;
    while (n + 1 < sizeof line && read(fd, &byte, 1) == 1 && byte != '\n')
        line[n++] = byte;
    line[n] = 0;
    CHECK(strcmp(line, expected) == 0, "a child said \"%s\", not \"%s\"", line, expected);
}

#endif
