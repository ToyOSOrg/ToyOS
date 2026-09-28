/* `struct stat` as the C library fills it: the size and the mtime a C caller
   reads are the file's, which they are only if libc's layout is the header's. */
#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#define PATH "/tmp/202_stat_mtime"
#define BYTES 17

static void judge(const char *how, const struct stat *st, time_t before, time_t after) {
    printf("%s st_size %ld\n", how, (long)st->st_size);
    if (before <= st->st_mtime && st->st_mtime <= after)
        printf("%s st_mtime within the write\n", how);
    else
        printf("%s st_mtime %ld outside [%ld, %ld]\n", how, (long)st->st_mtime, (long)before,
               (long)after);
    if (0 <= st->st_mtim.tv_nsec && st->st_mtim.tv_nsec < 1000000000L)
        printf("%s tv_nsec below a second\n", how);
    else
        printf("%s tv_nsec %ld\n", how, (long)st->st_mtim.tv_nsec);
}

int main(void) {
    struct stat st;
    time_t before, after;
    int fd;

    printf("sizeof(struct stat) %zu\n", sizeof(struct stat));

    before = time(NULL);
    fd = open(PATH, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0 || write(fd, "seventeen bytes!\n", BYTES) != BYTES) {
        printf("could not write " PATH "\n");
        return 1;
    }
    if (fstat(fd, &st) != 0) {
        printf("fstat failed\n");
        return 1;
    }
    close(fd);
    after = time(NULL);
    judge("fstat", &st, before, after);

    if (stat(PATH, &st) != 0) {
        printf("stat failed\n");
        return 1;
    }
    judge("stat", &st, before, after);
    return 0;
}
