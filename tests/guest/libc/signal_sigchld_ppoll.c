/* A child's end raises SIGCHLD once. With SIGCHLD blocked, a ppoll that
   unblocks it answers EINTR once the child ends, its handler run; a second,
   the child unwaited, answers the child's readable pipe; and a third, nothing
   ready, blocks until a second child, which has not ended, writes. */
#include <poll.h>
#include <signal.h>
#include <time.h>

#include "check.h"

#define SELF "/system/bin/test_c_signal_sigchld_ppoll"

static volatile sig_atomic_t raised;

static void on_child(int sig) {
    (void)sig;
    raised++;
}

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "say") == 0) {
        printf("said\n");
        return 0;
    }
    if (argc == 3 && strcmp(argv[1], "say-held") == 0) {
        char byte;
        CHECK(read(0, &byte, 1) == 1, "the go-ahead");
        printf("said\n");
        fflush(stdout);
        while (read(0, &byte, 1) > 0) {
        }
        return 0;
    }
    struct sigaction action = {.sa_handler = on_child};
    sigemptyset(&action.sa_mask);
    CHECK(sigaction(SIGCHLD, &action, NULL) == 0, "sigaction");
    sigset_t blocked, open;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGCHLD);
    CHECK(sigprocmask(SIG_BLOCK, &blocked, &open) == 0, "sigprocmask");

    int out[2];
    CHECK(pipe(out) == 0, "pipe");
    posix_spawn_file_actions_t actions;
    posix_spawn_file_actions_init(&actions);
    posix_spawn_file_actions_adddup2(&actions, out[1], 1);
    pid_t pid = spawn_role(SELF, "say", "-", &actions, NULL);
    posix_spawn_file_actions_destroy(&actions);
    close(out[1]);

    struct pollfd none = {.fd = -1};
    errno = 0;
    int got = ppoll(&none, 1, NULL, &open);
    CHECK(got == -1 && errno == EINTR && raised == 1, "the first ppoll answered %d, the handler run %d times", got, raised);

    struct pollfd said = {.fd = out[0], .events = POLLIN};
    got = ppoll(&said, 1, NULL, &open);
    CHECK(got == 1 && (said.revents & POLLIN) && raised == 1, "the second ppoll answered %d, the handler run %d times", got, raised);

    int out2[2], in2[2];
    CHECK(pipe(out2) == 0 && pipe(in2) == 0, "pipe");
    posix_spawn_file_actions_init(&actions);
    posix_spawn_file_actions_adddup2(&actions, out2[1], 1);
    posix_spawn_file_actions_adddup2(&actions, in2[0], 0);
    pid_t second = spawn_role(SELF, "say-held", "-", &actions, NULL);
    posix_spawn_file_actions_destroy(&actions);
    close(out2[1]);
    close(in2[0]);
    struct pollfd idle = {.fd = out2[0], .events = POLLIN};
    struct timespec zero = {0, 0};
    got = ppoll(&idle, 1, &zero, &open);
    CHECK(got == 0 && raised == 1, "the third ppoll found %d ready before the second child wrote", got);
    CHECK(write(in2[1], "g", 1) == 1, "the go-ahead");
    got = ppoll(&idle, 1, NULL, &open);
    CHECK(got == 1 && raised == 1, "the third ppoll answered %d, the handler run %d times", got, raised);
    close(in2[1]);

    CHECK(WIFEXITED(waited(pid)), "the child");
    CHECK(WIFEXITED(waited(second)), "the second child");
    printf("signal_sigchld_ppoll: SIGCHLD was raised once, and a ppoll unblocking it answered EINTR, then ready, then blocked until a live child wrote\n");
    return 0;
}
