/* The *at calls, positional reads and writes, and a file's times, through
   the C library: in a directory a file server holds (/home), by its
   descriptor, and in the kernel's /tmp by its path. A path from a directory
   descriptor is resolved from that directory, its `..` by the directory
   itself; a descriptor follows its directory across a rename. pread and
   pwrite never move the descriptor's offset, even while another thread
   reads or writes through it. What a volume keeps no record of -- a second name, an
   unnamed file, an access time -- is refused ENOTSUP, never answered as done. */
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <unistd.h>

#define HOME_DIR "/home/214_at"
#define TMP_FILE "/tmp/214_at_words"

/* Words in a file whose every word is its own offset. */
#define WORDS 4096

static void judge(const char *what, int is) {
    printf("%s: %s\n", what, is ? "yes" : "NO");
}

static int listed(int dirfd_, const char *name) {
    int fd = openat(dirfd_, ".", O_RDONLY | O_DIRECTORY);
    DIR *d = fd < 0 ? NULL : fdopendir(fd);
    struct dirent *e;
    int found = 0;
    while (d && (e = readdir(d)))
        found |= strcmp(e->d_name, name) == 0;
    if (d)
        closedir(d);
    return found;
}

/* `path` filled with WORDS words, each its own offset. */
static int words(const char *path) {
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    for (uint64_t i = 0; fd >= 0 && i < WORDS; i++) {
        uint64_t word = i * 8;
        if (write(fd, &word, 8) != 8)
            return -1;
    }
    return fd;
}

struct reader {
    int fd;
    int wrong;
};

/* Reads the whole file through the descriptor's own offset while the main
   thread preads it, and counts every word not at its offset. */
static void *read_through(void *arg) {
    struct reader *r = arg;
    uint64_t word;
    for (uint64_t at = 0; at < WORDS * 8; at += 8)
        if (read(r->fd, &word, 8) != 8 || word != at)
            r->wrong++;
    return NULL;
}

/* While another thread reads `path` through, pread sees each word at its
   offset and leaves that thread's offset alone. */
static int pread_beside_a_read(const char *path) {
    int fd = words(path);
    if (fd < 0 || lseek(fd, 0, SEEK_SET) != 0)
        return 0;
    struct reader r = { fd, 0 };
    pthread_t thread;
    if (pthread_create(&thread, NULL, read_through, &r) != 0)
        return 0;
    int wrong = 0;
    for (int pass = 0; pass < 8; pass++)
        for (uint64_t at = WORDS * 8 - 8;; at -= 8 * 61) {
            uint64_t word;
            if (pread(fd, &word, 8, at) != 8 || word != at)
                wrong++;
            if (at < 8 * 61)
                break;
        }
    pthread_join(thread, NULL);
    int ended = lseek(fd, 0, SEEK_CUR) == WORDS * 8;
    close(fd);
    return wrong == 0 && r.wrong == 0 && ended;
}

struct writer {
    int fd;
    int short_writes;
};

/* Writes every word but the ones pwrite puts in place, through the
   descriptor's own offset, while the main thread pwrites the rest. */
static void *write_through(void *arg) {
    struct writer *w = arg;
    for (uint64_t at = 0; at < WORDS * 8; at += 8) {
        uint64_t word = at % (8 * 7) == 0 ? ~(uint64_t)0 : at;
        if (write(w->fd, &word, 8) != 8)
            w->short_writes++;
    }
    return NULL;
}

/* While another thread writes `path` through, pwrite puts a word at its own
   offset and moves no offset: every word of the file ends as its own. The
   other thread's placeholder at a pwritten word may land before or after the
   pwrite, so the main thread puts those words again once it has joined, with
   pwrite, which a moved offset would still have sent astray. */
static int pwrite_beside_a_write(const char *path) {
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0)
        return 0;
    struct writer w = { fd, 0 };
    pthread_t thread;
    if (pthread_create(&thread, NULL, write_through, &w) != 0)
        return 0;
    int wrong = 0;
    for (uint64_t at = 0; at < WORDS * 8; at += 8 * 7)
        wrong += pwrite(fd, &at, 8, at) != 8;
    pthread_join(thread, NULL);
    for (uint64_t at = 0; at < WORDS * 8; at += 8 * 7)
        wrong += pwrite(fd, &at, 8, at) != 8;
    struct stat st;
    int whole = lseek(fd, 0, SEEK_CUR) == WORDS * 8 && fstat(fd, &st) == 0 && st.st_size == WORDS * 8;
    for (uint64_t at = 0; at < WORDS * 8; at += 8) {
        uint64_t word;
        wrong += pread(fd, &word, 8, at) != 8 || word != at;
    }
    close(fd);
    return wrong == 0 && w.short_writes == 0 && whole;
}

/* pwrite puts each word at its own offset in a file opened to append, and
   neither moves the offset nor appends. */
static int pwrite_at_its_offset(const char *path) {
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC | O_APPEND, 0644);
    if (fd < 0 || ftruncate(fd, WORDS * 8) != 0)
        return 0;
    int wrong = 0;
    for (uint64_t at = 0; at < WORDS * 8; at += 8 * 7)
        wrong += pwrite(fd, &at, 8, at) != 8;
    struct stat st;
    int unmoved = lseek(fd, 0, SEEK_CUR) == 0 && fstat(fd, &st) == 0 && st.st_size == WORDS * 8;
    for (uint64_t at = 0; at < WORDS * 8; at += 8 * 7) {
        uint64_t word;
        wrong += pread(fd, &word, 8, at) != 8 || word != at;
    }
    close(fd);
    return wrong == 0 && unmoved;
}

int main(void) {
    struct stat st, st2;
    char buf[64];
    int dir, sub, fd, n;

    /* What a run before this one left. */
    unlink(HOME_DIR "/moved/inner/f");
    unlink(HOME_DIR "/moved/inner/made");
    rmdir(HOME_DIR "/moved/inner");
    unlink(HOME_DIR "/moved/l");
    rmdir(HOME_DIR "/moved");
    unlink(HOME_DIR "/words");
    unlink(HOME_DIR "/appended");
    unlink(HOME_DIR "/written");
    unlink(HOME_DIR "/g");
    rmdir(HOME_DIR);

    judge("mkdir makes the directory", mkdir(HOME_DIR, 0755) == 0);
    dir = open(HOME_DIR, O_RDONLY | O_DIRECTORY);
    judge("open O_DIRECTORY opens a directory", dir >= 0);
    judge("fstat of it is a directory", fstat(dir, &st) == 0 && S_ISDIR(st.st_mode));
    judge("read of it is EISDIR", read(dir, buf, 1) == -1 && errno == EISDIR);
    judge("open O_DIRECTORY of a file is ENOTDIR",
          (fd = open("/home/214_at_absent_file", O_RDWR | O_CREAT, 0644)) >= 0 && close(fd) == 0 &&
              open("/home/214_at_absent_file", O_RDONLY | O_DIRECTORY) == -1 && errno == ENOTDIR &&
              unlink("/home/214_at_absent_file") == 0);

    judge("mkdirat makes a directory in it", mkdirat(dir, "sub", 0755) == 0);
    sub = openat(dir, "sub", O_RDONLY | O_DIRECTORY);
    judge("openat opens it", sub >= 0);
    fd = openat(sub, "f", O_WRONLY | O_CREAT | O_EXCL, 0644);
    judge("openat makes a file from it", fd >= 0 && write(fd, "at", 2) == 2 && close(fd) == 0);
    judge("fstatat finds the file from the parent", fstatat(dir, "sub/f", &st, 0) == 0 && S_ISREG(st.st_mode) && st.st_size == 2);
    judge("the file has one name", st.st_nlink == 1);
    judge("fstatat with AT_EMPTY_PATH is the descriptor's own", fstatat(sub, "", &st2, AT_EMPTY_PATH) == 0 && S_ISDIR(st2.st_mode));
    judge("fstatat of the empty path without it is ENOENT", fstatat(sub, "", &st2, 0) == -1 && errno == ENOENT);
    judge("a path from a descriptor that is no directory is ENOTDIR",
          (fd = openat(sub, "f", O_RDONLY)) >= 0 && fstatat(fd, "x", &st2, 0) == -1 && errno == ENOTDIR && close(fd) == 0);
    judge("fstatat of a bad descriptor is EBADF", fstatat(-5, "x", &st2, 0) == -1 && errno == EBADF);
    judge("an absolute path ignores the descriptor", fstatat(-5, HOME_DIR "/sub/f", &st2, 0) == 0 && st2.st_ino == st.st_ino);

    fd = openat(sub, "../sub/./f", O_RDONLY);
    n = fd < 0 ? -1 : read(fd, buf, sizeof buf);
    judge("a `..` from the directory climbs to its parent", n == 2 && memcmp(buf, "at", 2) == 0);
    close(fd);
    judge("a `..` above the directory's root is EACCES", fstatat(dir, "../../..", &st2, 0) == -1 && errno == EACCES);
    judge("an empty path from a descriptor is ENOENT",
          openat(dir, "", O_RDONLY) == -1 && errno == ENOENT && unlinkat(sub, "", AT_REMOVEDIR) == -1 && errno == ENOENT &&
              mkdirat(dir, "", 0755) == -1 && errno == ENOENT);
    judge("the directory is still there", fstatat(sub, "", &st2, AT_EMPTY_PATH) == 0 && S_ISDIR(st2.st_mode));
    judge("open of the empty path is ENOENT", open("", O_RDONLY) == -1 && errno == ENOENT);

    judge("symlinkat makes a link from the descriptor", symlinkat("f", sub, "l") == 0);
    n = readlinkat(sub, "l", buf, sizeof buf);
    judge("readlinkat reads it", n == 1 && buf[0] == 'f');
    judge("fstatat follows it", fstatat(sub, "l", &st2, 0) == 0 && S_ISREG(st2.st_mode));
    judge("fstatat AT_SYMLINK_NOFOLLOW is the link", fstatat(sub, "l", &st2, AT_SYMLINK_NOFOLLOW) == 0 && S_ISLNK(st2.st_mode));

    judge("unlinkat of a directory without AT_REMOVEDIR is refused", unlinkat(dir, "sub", 0) == -1);
    judge("renameat moves the file to the parent", renameat(sub, "f", dir, "g") == 0);
    judge("the file is in the parent's listing", listed(dir, "g") && !listed(sub, "f"));
    judge("renameat moves the directory", renameat(dir, "sub", dir, "moved") == 0);
    judge("its descriptor follows it", mkdirat(sub, "inner", 0755) == 0 && fstatat(dir, "moved/inner", &st2, 0) == 0);
    judge("unlinkat AT_REMOVEDIR of a full directory is ENOTEMPTY",
          unlinkat(dir, "moved", AT_REMOVEDIR) == -1 && errno == ENOTEMPTY);
    judge("unlinkat AT_REMOVEDIR removes an empty one", unlinkat(sub, "inner", AT_REMOVEDIR) == 0);
    judge("unlinkat removes the link", unlinkat(sub, "l", 0) == 0);
    judge("dirfd answers the stream's descriptor", ({
              DIR *d = opendir(HOME_DIR);
              int ok = d && dirfd(d) >= 0 && fstat(dirfd(d), &st2) == 0 && S_ISDIR(st2.st_mode);
              if (d)
                  closedir(d);
              ok;
          }));

    /* What the server's volume keeps no record of. */
    judge("link is ENOTSUP", link(HOME_DIR "/g", HOME_DIR "/g2") == -1 && errno == ENOTSUP);
    judge("linkat is ENOTSUP", linkat(dir, "g", sub, "g2", 0) == -1 && errno == ENOTSUP);
    judge("O_TMPFILE is ENOTSUP", openat(dir, ".", O_TMPFILE | O_RDWR, 0600) == -1 && errno == ENOTSUP);
    judge("O_TMPFILE to read alone is EINVAL", openat(dir, ".", O_TMPFILE | O_RDONLY, 0600) == -1 && errno == EINVAL);

    /* A modification time set through the path and through the descriptor. */
    struct timespec times[2] = { { 0, UTIME_OMIT }, { 1000000000, 123456789 } };
    judge("utimensat sets the modification time", utimensat(dir, "g", times, 0) == 0);
    judge("stat reads it back to the nanosecond",
          stat(HOME_DIR "/g", &st2) == 0 && st2.st_mtim.tv_sec == 1000000000 && st2.st_mtim.tv_nsec == 123456789);
    fd = openat(dir, "g", O_RDONLY);
    times[1].tv_sec = 1100000000;
    times[1].tv_nsec = 0;
    judge("futimens sets it through a descriptor", fd >= 0 && futimens(fd, times) == 0);
    judge("fstat reads that back", fstat(fd, &st2) == 0 && st2.st_mtim.tv_sec == 1100000000);
    close(fd);
    times[1].tv_nsec = UTIME_NOW;
    judge("UTIME_NOW sets it to now", utimensat(dir, "g", times, 0) == 0 && stat(HOME_DIR "/g", &st2) == 0 &&
                                          st2.st_mtim.tv_sec > 1100000000);
    judge("a tv_nsec past a second is EINVAL", ({
              struct timespec bad[2] = { { 0, UTIME_OMIT }, { 0, 1000000000 } };
              utimensat(dir, "g", bad, 0) == -1 && errno == EINVAL;
          }));
    judge("an access time the volume keeps none of is ENOTSUP", utimensat(dir, "g", NULL, 0) == -1 && errno == ENOTSUP);
    judge("O_DIRECTORY of a /tmp file is ENOTDIR", ({
              int t = open(TMP_FILE, O_RDWR | O_CREAT, 0644);
              close(t);
              open(TMP_FILE, O_RDONLY | O_DIRECTORY) == -1 && errno == ENOTDIR;
          }));
    judge("O_DIRECTORY of /tmp to write is EISDIR", open("/tmp", O_RDWR | O_DIRECTORY) == -1 && errno == EISDIR);
    judge("opendir of a /tmp file is ENOTDIR", opendir(TMP_FILE) == NULL && errno == ENOTDIR);
    judge("utimes of the kernel's /tmp is ENOTSUP", ({
              int t = open(TMP_FILE, O_RDWR | O_CREAT, 0644);
              close(t);
              utimes(TMP_FILE, NULL) == -1 && errno == ENOTSUP;
          }));

    judge("pread beside a read on /home", pread_beside_a_read(HOME_DIR "/words"));
    judge("pread beside a read on /tmp", pread_beside_a_read(TMP_FILE));
    judge("pwrite at its offset on /home", pwrite_at_its_offset(HOME_DIR "/appended"));
    judge("pwrite at its offset on /tmp", pwrite_at_its_offset(TMP_FILE));
    judge("pwrite beside a write on /home", pwrite_beside_a_write(HOME_DIR "/written"));
    judge("pwrite beside a write on /tmp", pwrite_beside_a_write(TMP_FILE));
    judge("pread of a negative offset is EINVAL", ({
              int t = open(TMP_FILE, O_RDONLY);
              int ok = pread(t, buf, 1, -1) == -1 && errno == EINVAL;
              close(t);
              ok;
          }));

    unlinkat(dir, "words", 0);
    unlinkat(dir, "appended", 0);
    unlinkat(dir, "written", 0);
    unlinkat(dir, "g", 0);
    unlink(TMP_FILE);
    close(sub);
    judge("rmdir removes the moved directory", unlinkat(dir, "moved", AT_REMOVEDIR) == 0);
    close(dir);
    judge("rmdir removes the directory", rmdir(HOME_DIR) == 0);
    return 0;
}
