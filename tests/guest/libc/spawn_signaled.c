/* A killed child reads WIFSIGNALED with SIGKILL, and a child that faults
   with its <signal.h> class's signal. */
#include <signal.h>

#include "check.h"

#define SELF "/system/bin/test_c_spawn_signaled"

static void fault(const char *kind) {
    if (strcmp(kind, "memory") == 0) {
        *(volatile int *)0 = 1;
    } else if (strcmp(kind, "instruction") == 0) {
#if defined(__x86_64__)
        __asm__ volatile("ud2");
#else
        __asm__ volatile("udf #0");
#endif
    } else if (strcmp(kind, "arithmetic") == 0) {
        volatile int zero = 0;
        volatile int one = 1;
        one = one / zero;
    } else if (strcmp(kind, "trap") == 0) {
#if defined(__x86_64__)
        /* RFLAGS.TF: the instruction after the popfq traps. */
        __asm__ volatile("pushfq; orq $0x100, (%rsp); popfq; nop");
#else
        __asm__ volatile("brk #0");
#endif
    }
}

static const struct {
    const char *kind;
    int signal;
} FAULTS[] = {
    {"memory", SIGSEGV},
    {"instruction", SIGILL},
#if defined(__x86_64__)
    /* AArch64 answers an integer division by zero with zero. */
    {"arithmetic", SIGFPE},
#endif
    {"trap", SIGTRAP},
};

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "fault") == 0) {
        fault(argv[2]);
        return 0;
    }
    if (argc >= 2 && strcmp(argv[1], "held") == 0) {
        char byte;
        while (read(0, &byte, 1) > 0) {
        }
        return 0;
    }

    int ends[2];
    CHECK(pipe(ends) == 0, "pipe");
    posix_spawn_file_actions_t actions;
    CHECK(posix_spawn_file_actions_init(&actions) == 0, "refused");
    CHECK(posix_spawn_file_actions_adddup2(&actions, ends[0], 0) == 0, "refused");
    pid_t held = spawn_role(SELF, "held", "", &actions, NULL);
    posix_spawn_file_actions_destroy(&actions);
    close(ends[0]);
    CHECK(kill(held, SIGKILL) == 0, "kill(%d, SIGKILL)", held);
    int status = waited(held);
    CHECK(WIFSIGNALED(status) && WTERMSIG(status) == SIGKILL, "the killed child's status %#x", status);
    close(ends[1]);

    for (size_t i = 0; i < sizeof FAULTS / sizeof FAULTS[0]; i++) {
        int status = waited(spawn_role(SELF, "fault", FAULTS[i].kind, NULL, NULL));
        CHECK(WIFSIGNALED(status) && WTERMSIG(status) == FAULTS[i].signal, "a %s fault's status %#x, and its signal is %d",
              FAULTS[i].kind, status, FAULTS[i].signal);
    }
    printf("spawn_signaled: a killed child read SIGKILL, and each fault its class's signal\n");
    return 0;
}
