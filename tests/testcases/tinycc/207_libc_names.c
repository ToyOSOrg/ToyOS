/* What libc does for the names LLVM builds against: creat and readlink as
   POSIX has them, each read back; a directory listed with the name
   and kind of each entry; dladdr naming the image and the exported symbol an
   address lies in; and strnlen, strsignal, modf, logb and the <endian.h>
   conversions. */
#include <dirent.h>
#include <dlfcn.h>
#include <endian.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <link.h>
#include <math.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sysexits.h>
#include <unistd.h>

#define DIR_PATH "/tmp/207_libc_names"
#define TARGET DIR_PATH "/target"
#define LISTED DIR_PATH "/listed"
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

/* A call that answers -1 and sets errno: what it answered, and errno. */
static void refused(const char *what, long answer) {
    printf("%s: %ld, %s\n", what, answer, errno_name(errno));
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
    fd = open(TARGET, O_RDONLY);
    ssize_t n = fd < 0 ? -1 : read(fd, buf, sizeof buf);
    printf("read what creat wrote: \"%.*s\"\n", n > 0 ? (int)n : 0, buf);
    if (fd >= 0)
        close(fd);

    refused("readlink of a file", readlink(TARGET, buf, sizeof buf));
    refused("readlink of nothing", readlink(DIR_PATH "/none", buf, sizeof buf));
    refused("readlink into 0", readlink(TARGET, buf, 0));
    refused("readlink into SIZE_MAX", readlink(DIR_PATH "/none", buf, SIZE_MAX));
}

static void listing(void) {
    if (mkdir(LISTED, 0755) != 0 || mkdir(LISTED "/sub", 0755) != 0 || close(creat(LISTED "/a", 0644)) != 0
        || close(creat(LISTED "/b", 0644)) != 0 || close(creat(LISTED "/sub/inner", 0644)) != 0) {
        printf("could not make %s\n", LISTED);
        return;
    }
    DIR *dir = opendir(LISTED);
    if (!dir) {
        refused("opendir", -1);
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
    refused("opendir of nothing", opendir(LISTED "/none") == NULL ? -1 : 0);
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
    printf("logb: %g %g\n", logb(0.1), logb(1024.0));
    errno = 0;
    double pole = logb(0.0);
    printf("logb(0): %g, %s\n", pole, errno == ERANGE ? "ERANGE" : errno_name(errno));
    printf("endian: %s 0x%x 0x%x\n", BYTE_ORDER == LITTLE_ENDIAN ? "little" : "big",
           (unsigned)htobe32(0x01020304), (unsigned)le16toh(0x0102));

    /* The rest of what LLVM reads of these headers, by value and by size. */
    struct sockaddr_un local = { AF_UNIX, "" };
    printf("names: AF_UNIX %d, sockaddr_un %zu, EX_IOERR %d, _POSIX_ARG_MAX %d, page %ld\n", local.sun_family,
           sizeof local, EX_IOERR, _POSIX_ARG_MAX, sysconf(_SC_PAGE_SIZE));
    printf("names: SA_ONSTACK|SA_NODEFER|SA_RESETHAND 0x%lx, F_RDLCK %d, F_UNLCK %d, rusage %zu, RLIM_INFINITY %s\n",
           (unsigned long)(SA_ONSTACK | SA_NODEFER | SA_RESETHAND), F_RDLCK, F_UNLCK, sizeof(struct rusage),
           RLIM_INFINITY == ~0UL ? "every bit" : "not every bit");
    return 0;
}
