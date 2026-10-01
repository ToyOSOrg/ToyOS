/* A thread's signal mask, as pthread_sigmask and sigprocmask keep it: each
   change read back by the next call, SIGKILL and SIGSTOP never in it, a how
   that is none of POSIX's three refused, and a new thread starting with its
   creator's mask; and a set as sigemptyset, sigaddset and sigfillset make
   one, a number no signal has refused. Signal n is bit n - 1 of a
   sigset_t. */
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>

#define BIT(n) (1UL << ((n) - 1))

static sigset_t current(void) {
    sigset_t now = 0xdead;
    pthread_sigmask(SIG_BLOCK, NULL, &now);
    return now;
}

static void *child(void *arg) {
    *(sigset_t *)arg = current();
    return NULL;
}

/* One change: what the call answered, the mask it said was there, and the
   mask the next call reads. */
static void changed(const char *what, int answer, sigset_t old) {
    sigset_t now = current();
    printf("%s: %d, old 0x%lx, now 0x%lx\n", what, answer, old, now);
}

int main(void) {
    sigset_t usr1 = BIT(SIGUSR1), usr2 = BIT(SIGUSR2), all = ~0UL, none = 0, old;
    int answer;

    printf("at start: 0x%lx\n", current());
    old = 0xdead;
    answer = pthread_sigmask(SIG_BLOCK, &usr1, &old);
    changed("block USR1", answer, old);
    old = 0xdead;
    answer = sigprocmask(SIG_BLOCK, &usr2, &old);
    changed("block USR2", answer, old);
    old = 0xdead;
    answer = pthread_sigmask(SIG_UNBLOCK, &usr1, &old);
    changed("unblock USR1", answer, old);

    sigset_t seen = 0xdead;
    pthread_t thread;
    if (pthread_create(&thread, NULL, child, &seen) != 0 || pthread_join(thread, NULL) != 0) {
        printf("could not run a thread\n");
        return 1;
    }
    printf("a new thread starts with: 0x%lx\n", seen);

    pthread_sigmask(SIG_SETMASK, &all, NULL);
    sigset_t everything = current();
    printf("set all: KILL %s, STOP %s, the rest %s\n", everything & BIT(SIGKILL) ? "held" : "dropped",
           everything & BIT(SIGSTOP) ? "held" : "dropped",
           (everything | BIT(SIGKILL) | BIT(SIGSTOP)) == all ? "held" : "not all held");

    answer = pthread_sigmask(99, &usr1, NULL);
    printf("how 99: %s, the mask %s\n", answer == EINVAL ? "EINVAL" : "taken",
           current() == everything ? "as it was" : "changed");
    errno = 0;
    answer = sigprocmask(99, &usr1, NULL);
    printf("sigprocmask how 99: %d, %s\n", answer, errno == EINVAL ? "EINVAL" : "another errno");
    old = 0xdead;
    answer = pthread_sigmask(99, NULL, &old);
    printf("how 99 with no set: %d, old %s\n", answer, old == everything ? "the mask" : "another");
    old = 0xdead;
    answer = sigprocmask(SIG_SETMASK, &none, &old);
    changed("set none", answer, old == everything ? 1 : 0);

    sigset_t made = 0xdead;
    answer = sigemptyset(&made);
    printf("sigemptyset: %d, 0x%lx\n", answer, made);
    answer = sigaddset(&made, SIGUSR1);
    printf("sigaddset USR1: %d, 0x%lx\n", answer, made);
    errno = 0;
    answer = sigaddset(&made, 0);
    printf("sigaddset 0: %d, %s, 0x%lx\n", answer, errno == EINVAL ? "EINVAL" : "another errno", made);
    errno = 0;
    answer = sigaddset(&made, 65);
    printf("sigaddset 65: %d, %s, 0x%lx\n", answer, errno == EINVAL ? "EINVAL" : "another errno", made);
    answer = sigfillset(&made);
    printf("sigfillset: %d, 0x%lx\n", answer, made);
    return 0;
}
