/* A full descriptor table refuses an open with EMFILE, whoever serves the
   path: the kernel's /tmp and a file server's /home alike, and the served
   file the server opened is let go again rather than held. Duplicates of
   one /home descriptor fill the table, as they take no kernel handle;
   descriptors that are each a kernel handle meet the kernel's handle table
   first, which answers ENOMEM, the kernel's one word for a full table and
   for no memory. */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>

#define SERVED "/home/212_full_table"
#define KERNEL "/tmp/212_full_table"

static int made(const char *path) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    return fd >= 0 && close(fd) == 0;
}

/* Close every descriptor from 3 up to and including `last`. */
static void close_to(int last) {
    for (int fd = 3; fd <= last; fd++)
        close(fd);
}

int main(void) {
    int last = -1, fd;
    if (!made(SERVED) || !made(KERNEL)) {
        printf("could not make the two files\n");
        return 1;
    }
    int served = open(SERVED, O_RDONLY);
    while ((fd = dup(served)) >= 0)
        last = fd;
    printf("duplicates filled the table with EMFILE: %s\n", errno == EMFILE ? "yes" : "NO");
    errno = 0;
    printf("an open on /tmp is EMFILE: %s\n", open(KERNEL, O_RDONLY) == -1 && errno == EMFILE ? "yes" : "NO");
    errno = 0;
    printf("an open on /home is EMFILE: %s\n", open(SERVED, O_RDONLY) == -1 && errno == EMFILE ? "yes" : "NO");
    close(last);
    fd = open(SERVED, O_RDONLY);
    printf("with one free, /home opens: %s\n", fd >= 0 ? "yes" : "NO");
    close(fd);
    close_to(last);

    errno = 0;
    while ((fd = open("/dev/null", O_RDONLY)) >= 0)
        last = fd;
    printf("kernel handles ran out first with ENOMEM: %s\n", errno == ENOMEM ? "yes" : "NO");
    close_to(last);
    printf("both are removed: %s\n", unlink(SERVED) == 0 && unlink(KERNEL) == 0 ? "yes" : "NO");
    return 0;
}
