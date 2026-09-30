/* What libc does for the names LLVM builds against: creat, symlink and
   readlink as POSIX has them; a directory listed with the name and kind of
   each entry; dladdr naming the image and the exported symbol an address lies
   in; and strnlen, strsignal, modf, logb and the <endian.h> conversions. */
#include <dirent.h>
#include <dlfcn.h>
#include <endian.h>
#include <errno.h>
#include <fcntl.h>
#include <link.h>
#include <math.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define DIR_PATH "/tmp/207_libc_names"
#define TARGET DIR_PATH "/target"
#define LINK DIR_PATH "/link"
/* The image's test library, which exports tls_get_label. */
#define LIB "/system/lib/libtls_lib.so"

int main(void);

static const char *errno_name(int e) {
    switch (e) {
    case EEXIST: return "EEXIST";
    case EINVAL: return "EINVAL";
    case ENOENT: return "ENOENT";
    default: return "another errno";
    }
}

static int by_name(const void *a, const void *b) {
    return strcmp(*(char *const *)a, *(char *const *)b);
}

static void links(void) {
    char buf[64];
    int fd = creat(TARGET, 0644);
    if (fd < 0 || write(fd, "hello", 5) != 5 || close(fd) != 0) {
        printf("creat %s failed\n", TARGET);
        return;
    }
    printf("symlink: %d\n", symlink(TARGET, LINK));
    printf("symlink over the link: %d, %s\n", symlink(TARGET, LINK), errno_name(errno));
    printf("symlink over a file: %d, %s\n", symlink(LINK, TARGET), errno_name(errno));

    ssize_t n = readlink(LINK, buf, sizeof buf);
    printf("readlink: %zd, \"%.*s\"\n", n, n > 0 ? (int)n : 0, buf);
    memset(buf, '#', sizeof buf);
    n = readlink(LINK, buf, 4);
    printf("readlink into 4: %zd, \"%.5s\"\n", n, buf);
    printf("readlink of a file: %zd, %s\n", readlink(TARGET, buf, sizeof buf), errno_name(errno));
    printf("readlink of nothing: %zd, %s\n", readlink(DIR_PATH "/none", buf, sizeof buf), errno_name(errno));
    printf("readlink into 0: %zd, %s\n", readlink(LINK, buf, 0), errno_name(errno));

    fd = open(LINK, O_RDONLY);
    n = fd < 0 ? -1 : read(fd, buf, sizeof buf);
    printf("read through the link: \"%.*s\"\n", n > 0 ? (int)n : 0, buf);
    if (fd >= 0)
        close(fd);
}

static void listing(void) {
    if (mkdir(DIR_PATH "/sub", 0755) != 0 || close(creat(DIR_PATH "/sub/inner", 0644)) != 0) {
        printf("could not make %s/sub\n", DIR_PATH);
        return;
    }
    DIR *dir = opendir(DIR_PATH);
    if (!dir) {
        printf("opendir %s: %s\n", DIR_PATH, errno_name(errno));
        return;
    }
    char *names[16];
    int count = 0;
    struct dirent *entry;
    while (count < 16 && (entry = readdir(dir)) != NULL) {
        char line[300];
        snprintf(line, sizeof line, "%s %s", entry->d_name,
                 entry->d_type == DT_DIR ? "DT_DIR" : entry->d_type == DT_UNKNOWN ? "DT_UNKNOWN" : "another type");
        names[count++] = strdup(line);
    }
    closedir(dir);
    qsort(names, count, sizeof names[0], by_name);
    for (int i = 0; i < count; i++)
        printf("entry: %s\n", names[i]);
    errno = 0;
    printf("opendir of nothing: %s, %s\n", opendir(DIR_PATH "/none") ? "a stream" : "null", errno_name(errno));
}

static int first_image(struct dl_phdr_info *info, size_t size, void *data) {
    (void)size;
    uintptr_t low = UINTPTR_MAX;
    for (int i = 0; i < info->dlpi_phnum; i++)
        if (info->dlpi_phdr[i].p_type == PT_LOAD && info->dlpi_phdr[i].p_vaddr < low)
            low = info->dlpi_phdr[i].p_vaddr;
    *(uintptr_t *)data = info->dlpi_addr + (low & ~(uintptr_t)0xfff);
    return 1;
}

static void dl(void) {
    Dl_info info;
    uintptr_t start = 0;
    dl_iterate_phdr(first_image, &start);
    int found = dladdr((const void *)main, &info);
    printf("dladdr(main): %d; names this program: %s; starts where dl_iterate_phdr says: %s; symbol: %s\n",
           found, found && strstr(info.dli_fname, "test_c_207_libc_names") ? "yes" : "no",
           found && (uintptr_t)info.dli_fbase == start ? "yes" : "no",
           found && info.dli_sname ? info.dli_sname : "none");

    int local;
    printf("dladdr(a stack address): %d\n", dladdr(&local, &info));

    void *lib = dlopen(LIB, RTLD_NOW);
    void *code = lib ? dlsym(lib, "tls_get_label") : NULL;
    if (!code) {
        printf("could not load %s\n", LIB);
        return;
    }
    found = dladdr((const char *)code + 1, &info);
    printf("dladdr(tls_get_label + 1): %d, %s, %s at the symbol's address: %s\n", found,
           found ? info.dli_fname : "-", found && info.dli_sname ? info.dli_sname : "none",
           found && info.dli_saddr == code ? "yes" : "no");
}

int main(void) {
    if (mkdir(DIR_PATH, 0755) != 0) {
        printf("could not make %s\n", DIR_PATH);
        return 1;
    }
    links();
    listing();
    dl();

    printf("strnlen: %zu %zu %zu\n", strnlen("hello", 3), strnlen("hi", 10), strnlen("", 4));
    printf("strsignal: %s; %s\n", strsignal(SIGSEGV), strsignal(99));
    double whole;
    double part = modf(-3.25, &whole);
    printf("modf(-3.25): %g and %g\n", whole, part);
    printf("logb: %g %g %g\n", logb(0.1), logb(1024.0), logb(0.0));
    printf("endian: %s %#x %#x\n", BYTE_ORDER == LITTLE_ENDIAN ? "little" : "big",
           (unsigned)htobe32(0x01020304), (unsigned)le16toh(0x0102));
    return 0;
}
