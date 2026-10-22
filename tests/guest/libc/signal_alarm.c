/* An alarm reaches SIGALRM: a handler installed without SA_RESTART runs, and
   a wait4 on a child that has not ended answers EINTR; with SIGALRM blocked
   in every thread a due alarm leaves the process running and sigpending names
   it, until an unblock ends the process with 142; and with it ignored a due
   alarm leaves the process running. The blocked arm waits for sigpending to
   name it, which the harness's bound ends if it never does; the ignored arm
   is a claim that nothing happens, looked at a second past the alarm, so a
   guest late by more than that passes it without the alarm having come due. */
#include <signal.h>
#include <sys/resource.h>

#include "check.h"

#define SELF "/system/bin/test_c_signal_alarm"

static volatile sig_atomic_t rang;

static void on_alarm(int sig) {
    (void)sig;
    rang++;
}

static int held(void) {
    char byte;
    while (read(0, &byte, 1) > 0) {
    }
    return 0;
}

static int blocked(void) {
    sigset_t alarm_only;
    sigemptyset(&alarm_only);
    sigaddset(&alarm_only, SIGALRM);
    CHECK(sigprocmask(SIG_BLOCK, &alarm_only, NULL) == 0, "sigprocmask");
    alarm(1);
    sigset_t pending;
    do {
        usleep(10000);
        CHECK(sigpending(&pending) == 0, "sigpending");
    } while (sigismember(&pending, SIGALRM) != 1);
    sigprocmask(SIG_UNBLOCK, &alarm_only, NULL);
    printf("signal_alarm: unblocking a pending SIGALRM left the process running\n");
    return 1;
}

static int ignored(void) {
    CHECK(signal(SIGALRM, SIG_IGN) != SIG_ERR, "signal(SIGALRM, SIG_IGN)");
    alarm(1);
    sleep(2);
    return 0;
}

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "held") == 0)
        return held();
    if (argc == 3 && strcmp(argv[1], "blocked") == 0)
        return blocked();
    if (argc == 3 && strcmp(argv[1], "ignored") == 0)
        return ignored();

    pid_t blocked_pid = spawn_role(SELF, "blocked", "-", NULL, NULL);
    pid_t ignored_pid = spawn_role(SELF, "ignored", "-", NULL, NULL);

    struct sigaction action = {.sa_handler = on_alarm};
    sigemptyset(&action.sa_mask);
    CHECK(sigaction(SIGALRM, &action, NULL) == 0, "sigaction");
    int stdin_pipe[2];
    CHECK(pipe(stdin_pipe) == 0, "pipe");
    posix_spawn_file_actions_t actions;
    posix_spawn_file_actions_init(&actions);
    posix_spawn_file_actions_adddup2(&actions, stdin_pipe[0], 0);
    pid_t child = spawn_role(SELF, "held", "-", &actions, NULL);
    posix_spawn_file_actions_destroy(&actions);
    close(stdin_pipe[0]);
    alarm(1);
    int status;
    struct rusage usage;
    errno = 0;
    pid_t got = wait4(child, &status, 0, &usage);
    CHECK(got == -1 && errno == EINTR && rang == 1, "wait4 across the alarm answered %d, the handler run %d times", got, rang);
    close(stdin_pipe[1]);
    CHECK(WIFEXITED(waited(child)), "the held child");

    int ended = waited(blocked_pid);
    CHECK(WIFEXITED(ended) && WEXITSTATUS(ended) == 142, "the blocked child's status %#x", ended);
    ended = waited(ignored_pid);
    CHECK(WIFEXITED(ended) && WEXITSTATUS(ended) == 0, "the ignoring child's status %#x", ended);
    printf("signal_alarm: SIGALRM's handler interrupted wait4, blocked it waited pending until its unblock ended the process 142, and ignored it did nothing\n");
    return 0;
}
