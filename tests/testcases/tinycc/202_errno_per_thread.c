/* errno is the calling thread's: a new thread's starts at zero whatever its
 * creator's holds, and a failure in one thread leaves another's untouched. */
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <sys/wait.h>

struct seen {
    int at_start;
    int after_failure;
    int differs;
};

struct child_arg {
    struct seen *seen;
    int *main_errno;
};

static void *child(void *arg)
{
    struct child_arg *in = arg;
    struct seen *seen = in->seen;
    seen->at_start = errno;
    /* No child exists, so this fails with ECHILD. */
    waitpid(-1, NULL, 0);
    seen->after_failure = errno;
    /* Compared here, while this thread's TLS is still alive: a pointer into
     * it is indeterminate once the thread has exited (C11 6.2.4p2). */
    seen->differs = (&errno != in->main_errno);
    return NULL;
}

int main(void)
{
    struct seen seen;
    struct child_arg arg;
    pthread_t thread;

    errno = ERANGE;
    arg.seen = &seen;
    arg.main_errno = &errno;
    if (pthread_create(&thread, NULL, child, &arg) != 0)
        return 1;
    pthread_join(thread, NULL);
    int mine = errno;

    printf("the new thread's errno starts at %d\n", seen.at_start);
    printf("its failure sets it to ECHILD: %s\n", seen.after_failure == ECHILD ? "yes" : "no");
    printf("main's errno is still ERANGE: %s\n", mine == ERANGE ? "yes" : "no");
    printf("the two are different objects: %s\n", seen.differs ? "yes" : "no");
    return 0;
}
