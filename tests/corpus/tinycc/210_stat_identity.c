/* `st_dev` and `st_ino` as the C library fills them: one file is one pair
   through every path, a link and its open descriptor, and across a rename;
   two files are two; and the two kernel mounts are two volumes. A kernel
   directory and symlink are each one as well, and `d_ino` is its entry's
   `st_ino` within one volume; a mount point's entry is not promised the
   mounted root's. */
#include <dirent.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int same(const struct stat *a, const struct stat *b) {
    return a->st_dev == b->st_dev && a->st_ino == b->st_ino;
}

static int identity(const char *path, struct stat *st, int follow) {
    if ((follow ? stat(path, st) : lstat(path, st)) != 0) {
        printf("stat %s failed\n", path);
        return 1;
    }
    if (st->st_dev == 0 || st->st_ino == 0) {
        printf("%s has no identity\n", path);
        return 1;
    }
    return 0;
}

static int stat_of(const char *path, struct stat *st) {
    return identity(path, st, 1);
}

static int lstat_of(const char *path, struct stat *st) {
    return identity(path, st, 0);
}

/* The `d_ino` `dir`'s listing answers for `name`, 0 when it lists none. */
static ino_t listed(const char *dir, const char *name) {
    DIR *d = opendir(dir);
    struct dirent *e;
    ino_t ino = 0;
    while (d && (e = readdir(d)))
        if (strcmp(e->d_name, name) == 0)
            ino = e->d_ino;
    if (d)
        closedir(d);
    return ino;
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
    struct stat a, b, a2, opened, moved, toybox, cat, release;
    struct stat bin, bin2, etc, sys, tmpdir, catlink, cplink;
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
        stat_of("/system/etc/os-release", &release))
        return 1;
    judge("a link is the file it names", same(&toybox, &cat), 1);
    judge("two files on /system are two", same(&toybox, &release), 0);
    judge("/system and /tmp are two volumes", toybox.st_dev == a.st_dev, 0);

    if (mkdir("/tmp/210_d", 0755) != 0 || stat_of("/system/bin", &bin) || stat_of("/system/etc/../bin", &bin2) ||
        stat_of("/system/etc", &etc) || stat_of("/system", &sys) || stat_of("/tmp/210_d", &tmpdir))
        return 1;
    judge("two directories on /system are two", same(&bin, &etc), 0);
    judge("a directory and its mount's root are two", same(&bin, &sys), 0);
    judge("one directory through two paths is one", same(&bin, &bin2), 1);
    judge("a directory is on its mount's volume", bin.st_dev == toybox.st_dev && tmpdir.st_dev == a.st_dev, 1);
    judge("a directory is no file", same(&tmpdir, &b), 0);

    if (lstat_of("/system/bin/cat", &catlink) || lstat_of("/system/bin/cp", &cplink))
        return 1;
    judge("two links on /system are two", same(&catlink, &cplink), 0);
    judge("a link is not the file it names", same(&catlink, &toybox), 0);

    judge("d_ino is a directory's st_ino", listed("/system", "bin") == bin.st_ino, 1);
    judge("d_ino is a file's st_ino", listed("/system/bin", "toybox") == toybox.st_ino, 1);
    judge("d_ino is a link's st_ino", listed("/system/bin", "cat") == catlink.st_ino, 1);
    judge("d_ino on /tmp is st_ino", listed("/tmp", "210_b") == b.st_ino && listed("/tmp", "210_d") == tmpdir.st_ino, 1);

    rmdir("/tmp/210_d");
    unlink("/tmp/210_moved");
    unlink("/tmp/210_b");
    return 0;
}
