/* A quit reaches a C child as its signal: kill with SIGINT, SIGHUP or SIGTERM
   is a quit of the child, which libc raises in it.
   - SIGINT handled without SA_RESTART: a read of an empty pipe answers EINTR.
   - SIGHUP handled: pause returns after it, and a SIGTERM handler ends the
     child once asked after.
   - SIGINT ignored: the child runs on.
   - SIGINT blocked: it waits pending until sigsuspend unblocks it, and its
     default then ends the child with 130.
   - SIGINT handled while the child computes and calls nothing: the handler
     runs beside it, restores the default and raises, and the child ends 130.
   Each child says "armed" once its disposition is kept; the test asks only
   after that, and asks a second time only after the child said it took the
   first. */
#include <signal.h>

#include "check.h"

#define SELF "/system/bin/test_c_signal_quit"

static volatile sig_atomic_t taken;

static void take(int sig) {
    (void)sig;
    taken++;
}

static void armed(void) {
    printf("armed\n");
    fflush(stdout);
}

static void keep(int sig, void (*handler)(int), int flags) {
    struct sigaction action = {.sa_handler = handler, .sa_flags = flags};
    sigemptyset(&action.sa_mask);
    CHECK(sigaction(sig, &action, NULL) == 0, "sigaction(%d)", sig);
}

static int reads_eintr(void) {
    keep(SIGINT, take, 0);
    armed();
    char byte;
    errno = 0;
    ssize_t got = read(0, &byte, 1);
    CHECK(got == -1 && errno == EINTR && taken == 1, "the read answered %zd, the handler run %d times", got, (int)taken);
    return 0;
}

static volatile sig_atomic_t ended;

static void end(int sig) {
    (void)sig;
    ended = 1;
}

static int reloads(void) {
    keep(SIGHUP, take, 0);
    keep(SIGTERM, end, 0);
    armed();
    errno = 0;
    CHECK(pause() == -1 && errno == EINTR && taken == 1, "pause across the hang-up");
    printf("reloaded\n");
    fflush(stdout);
    while (!ended)
        pause();
    return 0;
}

static int ignores(void) {
    CHECK(signal(SIGINT, SIG_IGN) != SIG_ERR, "signal(SIGINT, SIG_IGN)");
    armed();
    char byte;
    while (read(0, &byte, 1) > 0) {
    }
    return 0;
}

static int blocks(void) {
    sigset_t interrupt, none;
    sigemptyset(&interrupt);
    sigaddset(&interrupt, SIGINT);
    sigemptyset(&none);
    CHECK(sigprocmask(SIG_BLOCK, &interrupt, NULL) == 0, "sigprocmask");
    armed();
    sigsuspend(&none);
    CHECK(0, "sigsuspend returned past SIGINT's default");
    return 1;
}

static void restore_and_raise(int sig) {
    signal(sig, SIG_DFL);
    raise(sig);
}

static int computes(void) {
    keep(SIGINT, restore_and_raise, 0);
    armed();
    for (volatile unsigned long spin = 0;; spin++) {
    }
}

/* A child in `role`, its stdout a pipe answered in `out` and its stdin one
   held in `in`, past its "armed". */
static pid_t start(const char *role, int *out, int *in) {
    int said[2], stdin_pipe[2];
    CHECK(pipe(said) == 0 && pipe(stdin_pipe) == 0, "pipe");
    posix_spawn_file_actions_t actions;
    posix_spawn_file_actions_init(&actions);
    posix_spawn_file_actions_adddup2(&actions, said[1], 1);
    posix_spawn_file_actions_adddup2(&actions, stdin_pipe[0], 0);
    pid_t pid = spawn_role(SELF, role, "-", &actions, NULL);
    posix_spawn_file_actions_destroy(&actions);
    close(said[1]);
    close(stdin_pipe[0]);
    *out = said[0];
    *in = stdin_pipe[1];
    expect_line(*out, "armed");
    return pid;
}

int main(int argc, char **argv) {
    if (argc == 3) {
        const char *role = argv[1];
        if (strcmp(role, "reads") == 0)
            return reads_eintr();
        if (strcmp(role, "reloads") == 0)
            return reloads();
        if (strcmp(role, "ignores") == 0)
            return ignores();
        if (strcmp(role, "blocks") == 0)
            return blocks();
        if (strcmp(role, "computes") == 0)
            return computes();
    }
    int out, in, status;

    pid_t pid = start("reads", &out, &in);
    CHECK(kill(pid, SIGINT) == 0, "kill(SIGINT)");
    status = waited(pid);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "the reading child's status %#x", status);

    pid = start("reloads", &out, &in);
    CHECK(kill(pid, SIGHUP) == 0, "kill(SIGHUP)");
    expect_line(out, "reloaded");
    CHECK(kill(pid, SIGTERM) == 0, "kill(SIGTERM)");
    status = waited(pid);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "the reloading child's status %#x", status);

    pid = start("ignores", &out, &in);
    CHECK(kill(pid, SIGINT) == 0, "kill(SIGINT)");
    close(in);
    status = waited(pid);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "the ignoring child's status %#x", status);

    pid = start("blocks", &out, &in);
    CHECK(kill(pid, SIGINT) == 0, "kill(SIGINT)");
    status = waited(pid);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 128 + SIGINT, "the blocking child's status %#x", status);

    pid = start("computes", &out, &in);
    CHECK(kill(pid, SIGINT) == 0, "kill(SIGINT)");
    status = waited(pid);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 128 + SIGINT, "the computing child's status %#x", status);

    printf("signal_quit: a quit reached each child as its signal: EINTR, a reload then an end, an ignore, a pending default, and a handler beside a busy child\n");
    return 0;
}
