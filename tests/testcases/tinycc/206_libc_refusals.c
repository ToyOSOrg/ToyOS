/* What libc refuses: each call answers failure in its own POSIX form, errno
   says why, and nothing is done: ENOSYS where ToyOS lacks the function, and
   the errno POSIX names for a lock or a mapping it cannot take, or memory it
   cannot give. */
#include <errno.h>
#include <fcntl.h>
#include <pwd.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <unistd.h>

#define FILE_PATH "/tmp/206_libc_refusals"

static const char *errno_name(int e) {
    switch (e) {
    case 0: return "no errno";
    case ENOSYS: return "ENOSYS";
    case EINVAL: return "EINVAL";
    case ENODEV: return "ENODEV";
    case ENOTSUP: return "ENOTSUP";
    case ENOMEM: return "ENOMEM";
    default: return "another errno";
    }
}

/* One refusal: what the call answered, and what errno says. */
static void said(const char *call, long answer) {
    printf("%s: %ld, %s\n", call, answer, errno_name(errno));
    errno = 0;
}

int main(void) {
    char *argv[] = { "shell", NULL };
    char *envp[] = { NULL };
    char buf[256];
    struct statvfs vfs;
    struct rlimit limit = { 0, 0 };
    struct passwd pw, *found = &pw;
    struct flock lock = { F_WRLCK, SEEK_SET, 0, 0, 0 };

    int fd = open(FILE_PATH, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0 || write(fd, "x", 1) != 1) {
        printf("could not make " FILE_PATH "\n");
        return 1;
    }
    /* Opened before anything is closed, so its number is its slot, which is
       what dup2 below takes. */
    int cloexec = open(FILE_PATH, O_RDONLY | O_CLOEXEC);
    char *page = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) {
        printf("could not map a page\n");
        return 1;
    }
    struct stat st;
    if (fstat(fd, &st) != 0) {
        printf("could not stat " FILE_PATH "\n");
        return 1;
    }
    mode_t mode = st.st_mode;
    errno = 0;

    said("execv", execv("/system/bin/shell", argv));
    said("execve", execve("/system/bin/shell", argv, envp));
    said("setsid", setsid());
    said("getsid", getsid(0));
    said("gethostname", gethostname(buf, sizeof buf));
    said("link", link(FILE_PATH, FILE_PATH ".link"));
    said("symlink", symlink(FILE_PATH, FILE_PATH ".symlink"));
    said("fchown", fchown(fd, 0, 0));
    said("chmod", chmod(FILE_PATH, 0600));
    said("fchmod", fchmod(fd, 0600));
    said("statvfs", statvfs("/tmp", &vfs));
    said("fstatvfs", fstatvfs(fd, &vfs));
    said("getrlimit", getrlimit(RLIMIT_STACK, &limit));
    said("setrlimit", setrlimit(RLIMIT_CORE, &limit));
    said("msync", msync(page, 4096, MS_SYNC));
    said("mprotect", mprotect(page, 4096, PROT_READ));
    said("realpath", realpath("/tmp", NULL) == NULL ? -1 : 0);
    said("fcntl F_SETLK", fcntl(fd, F_SETLK, &lock));
    said("fcntl F_SETLKW", fcntl(fd, F_SETLKW, &lock));
    said("fcntl F_GETLK", fcntl(fd, F_GETLK, &lock));
    /* Close-on-exec is kept per descriptor. F_DUPFD, the first call to
       duplicate anything, answers at or above its argument a descriptor
       fstat reads the file's one byte through, and not closed on exec. */
    printf("fcntl F_GETFD: %d\n", fcntl(fd, F_GETFD));
    int set = fcntl(fd, F_SETFD, FD_CLOEXEC);
    printf("fcntl F_SETFD FD_CLOEXEC: %d; F_GETFD: %d\n", set, fcntl(fd, F_GETFD));
    int duplicate = fcntl(fd, F_DUPFD, 10);
    struct stat through;
    int read_back = duplicate >= 0 && fstat(duplicate, &through) == 0 && through.st_size == 1;
    printf("fcntl F_DUPFD 10: %s; fstat of it: %s; F_GETFD of it: %d\n", duplicate >= 10 ? "10 or above" : "below 10",
           read_back ? "the file's one byte" : "not the file", fcntl(duplicate, F_GETFD));
    if (duplicate >= 0)
        close(duplicate);
    set = fcntl(fd, F_SETFD, 0);
    printf("fcntl F_SETFD 0: %d; F_GETFD: %d\n", set, fcntl(fd, F_GETFD));
    printf("open O_CLOEXEC; F_GETFD: %d\n", fcntl(cloexec, F_GETFD));
    int onto = dup2(fd, cloexec);
    printf("dup2 onto it: %s; F_GETFD: %d\n", onto == cloexec ? "answered it" : "answered another",
           fcntl(onto, F_GETFD));
    close(cloexec);
    said("fcntl F_DUPFD -1", fcntl(fd, F_DUPFD, -1));
    said("fcntl F_DUPFD 4096", fcntl(fd, F_DUPFD, 4096));
    said("fcntl F_DUPFD_CLOEXEC", fcntl(fd, F_DUPFD_CLOEXEC, 10));
    said("fcntl F_GETFL", fcntl(fd, F_GETFL));
    said("fcntl F_SETFL", fcntl(fd, F_SETFL, O_NONBLOCK));
    said("fcntl 12345", fcntl(fd, 12345));

    /* The _r lookups answer their error, and null for the entry. */
    int answer = getpwnam_r("root", &pw, buf, sizeof buf, &found);
    printf("getpwnam_r: %s, entry %s\n", errno_name(answer), found ? "set" : "null");
    found = &pw;
    answer = getpwuid_r(0, &pw, buf, sizeof buf, &found);
    printf("getpwuid_r: %s, entry %s\n", errno_name(answer), found ? "set" : "null");

    /* mmap: no file, private or shared, at its start or a later page; no
       executable page; no empty mapping. */
    said("mmap a file", mmap(NULL, 4096, PROT_READ, MAP_PRIVATE, fd, 0) == MAP_FAILED ? -1 : 0);
    said("mmap a file at a page", mmap(NULL, 4096, PROT_READ, MAP_PRIVATE, fd, 4096) == MAP_FAILED ? -1 : 0);
    said("mmap a file shared", mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0) == MAP_FAILED ? -1 : 0);
    said("mmap a file shared at a page", mmap(NULL, 4096, PROT_READ, MAP_SHARED, fd, 4096) == MAP_FAILED ? -1 : 0);
    said("mmap executable", mmap(NULL, 4096, PROT_READ | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) == MAP_FAILED ? -1 : 0);
    said("mmap nothing", mmap(NULL, 0, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) == MAP_FAILED ? -1 : 0);

    /* munmap: a whole mapping, by any length that ends in its last page, or
       nothing. Of two pages the first alone is refused and the second keeps
       its byte, as do the second alone, no bytes, and a byte past both. */
    char *pair = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (pair == MAP_FAILED) {
        printf("could not map two pages\n");
        return 1;
    }
    pair[0] = 'f';
    pair[4096] = 's';
    /* Nor does a fixed mapping of the first page replace the second. */
    said("mmap the first of two pages fixed",
         mmap(pair, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0) == MAP_FAILED ? -1 : 0);
    printf("pages after it: %c %c\n", pair[0], pair[4096]);
    said("munmap the first of two pages", munmap(pair, 4096));
    printf("pages after it: %c %c\n", pair[0], pair[4096]);
    said("munmap the second of two pages", munmap(pair + 4096, 4096));
    said("munmap no bytes", munmap(pair, 0));
    said("munmap a byte past two pages", munmap(pair, 8193));
    printf("pages after them: %c %c\n", pair[0], pair[4096]);
    said("munmap two pages by a length into the second", munmap(pair, 4097));
    said("munmap them again", munmap(pair, 8192));
    /* A length off a page maps the pages it reaches into, and is unmapped by
       that length or by those pages'. */
    char *odd = mmap(NULL, 5000, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    said("munmap 5000 bytes by 5000", odd == MAP_FAILED ? -2 : munmap(odd, 5000));
    odd = mmap(NULL, 5000, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    said("munmap 5000 bytes by 8192", odd == MAP_FAILED ? -2 : munmap(odd, 8192));

    /* The allocators: null and ENOMEM for a size no block has. Each answer is
       stored, so the compiler cannot fold an unused allocation to non-null. */
    void *volatile got = malloc(SIZE_MAX);
    said("malloc SIZE_MAX", got == NULL ? -1 : 0);
    got = calloc(SIZE_MAX, 2);
    said("calloc SIZE_MAX 2", got == NULL ? -1 : 0);
    char *block = malloc(16);
    got = realloc(block, SIZE_MAX);
    said("realloc SIZE_MAX", got == NULL ? -1 : 0);
    free(block);

    /* sysconf: -1 with errno untouched for a limit on what ToyOS lacks, and
       EINVAL for a name it does not know. */
    said("sysconf _SC_ARG_MAX", sysconf(_SC_ARG_MAX));
    said("sysconf _SC_GETPW_R_SIZE_MAX", sysconf(_SC_GETPW_R_SIZE_MAX));
    said("sysconf 12345", sysconf(12345));

    /* posix_madvise takes every advice and answers an error number; madvise
       takes Linux's hints and refuses its discard, which would have zeroed
       the page. */
    page[0] = 'x';
    printf("posix_madvise WILLNEED: %d\n", posix_madvise(page, 4096, POSIX_MADV_WILLNEED));
    printf("posix_madvise 99: %s\n", errno_name(posix_madvise(page, 4096, 99)));
    said("madvise WILLNEED", madvise(page, 4096, MADV_WILLNEED));
    said("madvise DONTNEED", madvise(page, 4096, MADV_DONTNEED));
    said("madvise off a page", madvise(page + 1, 4096, MADV_WILLNEED));
    said("madvise 99", madvise(page, 4096, 99));
    printf("page after madvise: %c\n", page[0]);

    /* Nothing refused was done: the file has its one byte, its mode through
       its name and its descriptor, and no second name; the page is still
       writable; the limit is as it was. */
    struct stat by_name;
    long size = fstat(fd, &st) == 0 ? (long)st.st_size : -1L;
    int named = stat(FILE_PATH, &by_name) == 0;
    printf("file: %ld bytes; mode %s through fstat, %s through stat; second name: %s\n", size,
           st.st_mode == mode ? "as it was" : "changed", named && by_name.st_mode == mode ? "as it was" : "changed",
           access(FILE_PATH ".link", F_OK) == 0 || access(FILE_PATH ".symlink", F_OK) == 0 ? "made" : "none");
    page[0] = 'y';
    printf("page after mprotect: %c\n", page[0]);
    printf("limit after getrlimit: %lu %lu\n", (unsigned long)limit.rlim_cur, (unsigned long)limit.rlim_max);
    close(fd);
    unlink(FILE_PATH);
    return 0;
}
