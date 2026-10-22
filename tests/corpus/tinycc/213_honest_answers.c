/* What libc says is true: a handler for a signal nothing raises is refused,
   while the default and ignoring are kept and read back; an
   exclusive create on /tmp refuses a name that is there; a file has one
   link; and strerror says what a code means. */
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define PATH "/tmp/213_honest_answers"

static void handler(int sig) { (void)sig; }

static void judge(const char *what, int is) {
    printf("%s: %s\n", what, is ? "yes" : "NO");
}

int main(void) {
    struct sigaction act = { .sa_handler = handler }, old;
    struct stat st;

    judge("sigaction of a handler is EINVAL", sigaction(SIGUSR1, &act, NULL) == -1 && errno == EINVAL);
    judge("signal of a handler is SIG_ERR with EINVAL", signal(SIGUSR2, handler) == SIG_ERR && errno == EINVAL);
    judge("ignoring SIGPIPE is kept", signal(SIGPIPE, SIG_IGN) == SIG_DFL);
    judge("and read back", sigaction(SIGPIPE, NULL, &old) == 0 && old.sa_handler == SIG_IGN);
    judge("the default is kept again", signal(SIGPIPE, SIG_DFL) == SIG_IGN);
    judge("SIGKILL's disposition is EINVAL", signal(SIGKILL, SIG_IGN) == SIG_ERR && errno == EINVAL);
    judge("ignoring SIGCHLD is kept", signal(SIGCHLD, SIG_IGN) == SIG_DFL && signal(SIGCHLD, SIG_DFL) == SIG_IGN);
    judge("no signal 0", sigaction(0, NULL, &old) == -1 && errno == EINVAL);
    judge("raise of a signal whose default ignores does nothing", raise(SIGCHLD) == 0);
    signal(SIGTERM, SIG_IGN);
    judge("raise of an ignored signal does nothing", raise(SIGTERM) == 0);
    signal(SIGTERM, SIG_DFL);

    unlink(PATH);
    int fd = open(PATH, O_WRONLY | O_CREAT | O_EXCL, 0644);
    judge("an exclusive create on /tmp makes the file", fd >= 0 && close(fd) == 0);
    judge("and refuses it again with EEXIST", open(PATH, O_WRONLY | O_CREAT | O_EXCL, 0644) == -1 && errno == EEXIST);
    judge("a file has one link", stat(PATH, &st) == 0 && st.st_nlink == 1);
    judge("so does a directory", stat("/tmp", &st) == 0 && st.st_nlink == 1);
    judge("it is removed", unlink(PATH) == 0);
    judge("strerror names EEXIST", strcmp(strerror(EEXIST), "File exists") == 0);
    judge("and a number no code has", strcmp(strerror(9999), "Unknown error") == 0);
    return 0;
}
