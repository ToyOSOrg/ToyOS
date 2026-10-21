/* A directory a file server holds, `/home`, through the C library as a C
   program reaches it: a file is made, written, read back at an offset,
   appended to through stdio, linked, resolved, renamed, listed and removed,
   and each refusal is POSIX's errno. */
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define DIR_PATH "/home/211_files"

static void judge(const char *what, int is) {
    printf("%s: %s\n", what, is ? "yes" : "NO");
}

/* The `d_type` `DIR_PATH`'s listing answers for `name`, -1 when it lists none. */
static int listed(const char *name) {
    DIR *d = opendir(DIR_PATH);
    struct dirent *e;
    int type = -1;
    while (d && (e = readdir(d)))
        if (strcmp(e->d_name, name) == 0)
            type = e->d_type;
    if (d)
        closedir(d);
    return type;
}

int main(void) {
    char buf[32], real[PATH_MAX];
    struct stat st;
    int fd, n;

    /* What a run before this one left. */
    unlink(DIR_PATH "/f");
    unlink(DIR_PATH "/g");
    unlink(DIR_PATH "/l");
    rmdir(DIR_PATH);

    judge("mkdir makes the directory", mkdir(DIR_PATH, 0755) == 0);
    fd = open(DIR_PATH "/f", O_WRONLY | O_CREAT | O_EXCL, 0644);
    judge("open makes the file", fd >= 0);
    judge("write writes it", write(fd, "served\n", 7) == 7);
    judge("close closes it", close(fd) == 0);
    judge("O_EXCL refuses it again with EEXIST",
          open(DIR_PATH "/f", O_WRONLY | O_CREAT | O_EXCL, 0644) == -1 && errno == EEXIST);

    fd = open(DIR_PATH "/f", O_RDONLY);
    n = read(fd, buf, sizeof buf);
    judge("read reads it back", n == 7 && memcmp(buf, "served\n", 7) == 0);
    judge("lseek moves the offset", lseek(fd, 1, SEEK_SET) == 1);
    n = read(fd, buf, sizeof buf);
    judge("read reads from the offset", n == 6 && memcmp(buf, "erved\n", 6) == 0);
    judge("fstat answers a regular file and its size", fstat(fd, &st) == 0 && S_ISREG(st.st_mode) && st.st_size == 7);
    judge("isatty answers no, with ENOTTY", isatty(fd) == 0 && errno == ENOTTY);
    judge("write to a file open to read is EBADF", write(fd, "x", 1) == -1 && errno == EBADF);
    close(fd);

    FILE *f = fopen(DIR_PATH "/f", "a");
    judge("fopen appends", f && fputs("more\n", f) >= 0 && fclose(f) == 0);
    judge("stat answers the appended size", stat(DIR_PATH "/f", &st) == 0 && st.st_size == 12);

    judge("symlink makes a link", symlink("f", DIR_PATH "/l") == 0);
    judge("symlink refuses a name that exists with EEXIST", symlink("f", DIR_PATH "/l") == -1 && errno == EEXIST);
    n = readlink(DIR_PATH "/l", buf, sizeof buf);
    judge("readlink answers its target", n == 1 && buf[0] == 'f');
    judge("readlink of a file is EINVAL", readlink(DIR_PATH "/f", buf, sizeof buf) == -1 && errno == EINVAL);
    judge("lstat answers the link", lstat(DIR_PATH "/l", &st) == 0 && S_ISLNK(st.st_mode));
    judge("stat follows it", stat(DIR_PATH "/l", &st) == 0 && S_ISREG(st.st_mode) && st.st_size == 12);
    judge("realpath resolves it", realpath(DIR_PATH "/./l", real) && strcmp(real, DIR_PATH "/f") == 0);

    judge("chdir enters the directory", chdir(DIR_PATH) == 0);
    judge("rename renames by a relative path", rename("f", "g") == 0);
    judge("the old name is gone with ENOENT", stat(DIR_PATH "/f", &st) == -1 && errno == ENOENT);
    judge("the listing has the file", listed("g") == DT_REG);
    judge("the listing has the link", listed("l") == DT_LNK);
    judge("rmdir of a full directory is ENOTEMPTY", rmdir(DIR_PATH) == -1 && errno == ENOTEMPTY);
    judge("chdir leaves it", chdir("/tmp") == 0);

    judge("unlink removes the file and the link", unlink(DIR_PATH "/g") == 0 && unlink(DIR_PATH "/l") == 0);
    judge("rmdir removes the directory", rmdir(DIR_PATH) == 0);
    judge("it is gone with ENOENT", stat(DIR_PATH, &st) == -1 && errno == ENOENT);
    return 0;
}
