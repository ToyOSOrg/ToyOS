/* What libc refuses: each call answers failure in its own POSIX form, errno
   says why, and nothing is done. ENOSYS where ToyOS lacks the function; mmap's
   own errno where it refuses a mapping. */
#include <errno.h>
#include <fcntl.h>
#include <pwd.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <sys/utsname.h>
#include <unistd.h>

#define FILE_PATH "/tmp/206_libc_refusals"

static const char *errno_name(int e) {
    switch (e) {
    case 0: return "no errno";
    case ENOSYS: return "ENOSYS";
    case EINVAL: return "EINVAL";
    case ENODEV: return "ENODEV";
    case ENOTSUP: return "ENOTSUP";
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
    struct utsname uts;
    struct statvfs vfs;
    struct rlimit limit = { 0, 0 };
    struct passwd pw, *found = &pw;
    struct flock lock = { F_WRLCK, SEEK_SET, 0, 0, 0 };

    int fd = open(FILE_PATH, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0 || write(fd, "x", 1) != 1) {
        printf("could not make " FILE_PATH "\n");
        return 1;
    }
    char *page = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) {
        printf("could not map a page\n");
        return 1;
    }
    errno = 0;

    said("alarm", alarm(1));
    said("execv", execv("/system/bin/shell", argv));
    said("execve", execve("/system/bin/shell", argv, envp));
    said("setsid", setsid());
    said("getsid", getsid(0));
    said("gethostname", gethostname(buf, sizeof buf));
    said("uname", uname(&uts));
    said("link", link(FILE_PATH, FILE_PATH ".link"));
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

    /* The _r lookups answer their error, and null for the entry. */
    int answer = getpwnam_r("root", &pw, buf, sizeof buf, &found);
    printf("getpwnam_r: %s, entry %s\n", errno_name(answer), found ? "set" : "null");
    found = &pw;
    answer = getpwuid_r(0, &pw, buf, sizeof buf, &found);
    printf("getpwuid_r: %s, entry %s\n", errno_name(answer), found ? "set" : "null");

    /* mmap: no file, no executable page, no empty mapping. */
    said("mmap a file", mmap(NULL, 4096, PROT_READ, MAP_PRIVATE, fd, 0) == MAP_FAILED ? -1 : 0);
    said("mmap a file shared", mmap(NULL, 4096, PROT_READ, MAP_SHARED, fd, 4096) == MAP_FAILED ? -1 : 0);
    said("mmap executable", mmap(NULL, 4096, PROT_READ | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) == MAP_FAILED ? -1 : 0);
    said("mmap nothing", mmap(NULL, 0, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) == MAP_FAILED ? -1 : 0);

    /* sysconf: -1 with errno untouched for a limit on what ToyOS lacks, and
       EINVAL for a name it does not know. */
    said("sysconf _SC_ARG_MAX", sysconf(_SC_ARG_MAX));
    said("sysconf _SC_GETPW_R_SIZE_MAX", sysconf(_SC_GETPW_R_SIZE_MAX));
    said("sysconf 12345", sysconf(12345));

    /* posix_madvise takes every advice and answers an error number. */
    printf("posix_madvise WILLNEED: %d\n", posix_madvise(page, 4096, POSIX_MADV_WILLNEED));
    printf("posix_madvise 99: %s\n", errno_name(posix_madvise(page, 4096, 99)));

    /* The file is as it was: its one byte, and no second name. */
    struct stat st;
    printf("file: %ld bytes; second name: %s\n", fstat(fd, &st) == 0 ? (long)st.st_size : -1L,
           access(FILE_PATH ".link", F_OK) == 0 ? "made" : "none");
    close(fd);
    unlink(FILE_PATH);
    return 0;
}
