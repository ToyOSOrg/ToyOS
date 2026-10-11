/* `st_dev` and `st_ino` as the C library fills them: one file is one pair
   through every path, a link and its open descriptor, and across a rename;
   two files are two; and the two kernel mounts are two volumes. */
#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>

static int same(const struct stat *a, const struct stat *b) {
    return a->st_dev == b->st_dev && a->st_ino == b->st_ino;
}

static int stat_of(const char *path, struct stat *st) {
    if (stat(path, st) != 0) {
        printf("stat %s failed\n", path);
        return 1;
    }
    if (st->st_dev == 0 || st->st_ino == 0) {
        printf("%s has no identity\n", path);
        return 1;
    }
    return 0;
}

static int write_file(const char *path) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0 || write(fd, "x", 1) != 1) {
        printf("could not write %s\n", path);
        return 1;
    }
    close(fd);
    return 0;
}

static void judge(const char *what, int is, int want) {
    printf("%s: %s\n", what, is == want ? "yes" : "NO");
}

int main(void) {
    struct stat a, b, a2, opened, moved, toybox, cat, supervisor;
    int fd;

    if (write_file("/tmp/210_a") || write_file("/tmp/210_b"))
        return 1;
    if (stat_of("/tmp/210_a", &a) || stat_of("/tmp/210_b", &b) || stat_of("/tmp/../tmp/./210_a", &a2))
        return 1;
    judge("two files on /tmp are two", same(&a, &b), 0);
    judge("one file through two paths is one", same(&a, &a2), 1);

    fd = open("/tmp/210_a", O_RDONLY);
    if (fd < 0 || fstat(fd, &opened) != 0) {
        printf("fstat of /tmp/210_a failed\n");
        return 1;
    }
    judge("its open descriptor is the file", same(&a, &opened), 1);
    if (rename("/tmp/210_a", "/tmp/210_moved") != 0 || stat_of("/tmp/210_moved", &moved))
        return 1;
    judge("a rename keeps the file", same(&a, &moved), 1);
    if (fstat(fd, &opened) != 0) {
        printf("fstat after the rename failed\n");
        return 1;
    }
    judge("its descriptor after the rename is the file", same(&moved, &opened), 1);
    close(fd);

    if (stat_of("/system/bin/toybox", &toybox) || stat_of("/system/bin/cat", &cat) ||
        stat_of("/system/bin/supervisor", &supervisor))
        return 1;
    judge("a link is the file it names", same(&toybox, &cat), 1);
    judge("two files on /system are two", same(&toybox, &supervisor), 0);
    judge("/system and /tmp are two volumes", toybox.st_dev == a.st_dev, 0);

    unlink("/tmp/210_moved");
    unlink("/tmp/210_b");
    return 0;
}
