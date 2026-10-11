/* What libc does for the names LLVM builds against: a directory listed with
   the name and kind of each entry; dladdr naming the image and the exported
   symbol an address lies in; a file truncated by its path; a stream on a
   buffer of its caller's, or none, sought and told by off_t; SIGALRM's
   action, read back whole, and an alarm armed and disarmed; and strnlen, strsignal, modf,
   lround, logb, pathconf and the <endian.h> conversions. */
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
/* A link the image holds, to /system/bin/toybox. */
#define LINK "/system/bin/cat"
/* The image's test library, which exports tls_get_label. */
#define LIB "/system/lib/libtls_lib.so"

/* The rest of what LLVM reads of these headers, by value and by size. */
_Static_assert(BYTE_ORDER == LITTLE_ENDIAN, "BYTE_ORDER");
_Static_assert(htobe32(0x01020304) == 0x04030201, "htobe32");
_Static_assert(le16toh(0x0102) == 0x0102, "le16toh");
_Static_assert(AF_UNIX == 1, "AF_UNIX");
_Static_assert(sizeof(struct sockaddr_un) == 110, "sockaddr_un");
_Static_assert(EX_IOERR == 74, "EX_IOERR");
_Static_assert(_POSIX_ARG_MAX == 4096, "_POSIX_ARG_MAX");
_Static_assert((SA_ONSTACK | SA_NODEFER | SA_RESETHAND) == 0xc8000000, "SA_ONSTACK|SA_NODEFER|SA_RESETHAND");
_Static_assert(F_RDLCK == 0 && F_UNLCK == 2, "F_RDLCK, F_UNLCK");
_Static_assert(sizeof(struct rusage) == 144, "rusage");
_Static_assert(RLIM_INFINITY == ~0UL, "RLIM_INFINITY");

int main(void);

static const char *errno_name(int e) {
    switch (e) {
    case EDOM: return "EDOM";
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

    n = readlink(LINK, buf, sizeof buf);
    printf("readlink: %zd, \"%.*s\"\n", n, n > 0 ? (int)n : 0, buf);
    memset(buf, '#', sizeof buf);
    n = readlink(LINK, buf, 4);
    printf("readlink into 4: %zd, \"%.5s\"\n", n, buf);
    refused("readlink into 0", readlink(LINK, buf, 0));
    refused("readlink of a file", readlink(TARGET, buf, sizeof buf));
    refused("readlink of nothing", readlink(DIR_PATH "/none", buf, sizeof buf));
}

static void truncated(void) {
    struct stat st;
    int answer = truncate(TARGET, 2);
    printf("truncate to 2: %d; stat size: %ld\n", answer, stat(TARGET, &st) == 0 ? (long)st.st_size : -1L);
    refused("truncate of nothing", truncate(DIR_PATH "/none", 0));
}

/* setbuf's buffer holds what is written until the stream is told or sought;
   with none, a write reaches the file at once. */
static void buffered(void) {
    static char buf[BUFSIZ];
    struct stat st;
    FILE *f = fopen(DIR_PATH "/buffered", "w+");
    if (!f) {
        printf("fopen buffered failed\n");
        return;
    }
    setbuf(f, buf);
    fputs("0123456789", f);
    long held = stat(DIR_PATH "/buffered", &st) == 0 ? (long)st.st_size : -1L;
    printf("setbuf: file %ld bytes, the buffer %s\n", held, memcmp(buf, "0123456789", 10) == 0 ? "holds them" : "not");
    long told = (long)ftello(f);
    int sought = fseeko(f, 3, SEEK_SET);
    long at = (long)ftello(f);
    int c = fgetc(f);
    printf("ftello: %ld; fseeko 3: %d; ftello: %ld; fgetc: %c\n", told, sought, at, c);
    fclose(f);

    f = fopen(DIR_PATH "/unbuffered", "w");
    if (!f) {
        printf("fopen unbuffered failed\n");
        return;
    }
    setbuf(f, NULL);
    fputs("abc", f);
    printf("setbuf NULL: file %ld bytes\n", stat(DIR_PATH "/unbuffered", &st) == 0 ? (long)st.st_size : -1L);
    fclose(f);
}

static void alarms(void) {
    void (*was)(int) = signal(SIGALRM, SIG_IGN);
    void (*then)(int) = signal(SIGALRM, SIG_DFL);
    printf("signal SIGALRM: was %s, then %s\n", was == SIG_DFL ? "SIG_DFL" : "another",
           then == SIG_IGN ? "SIG_IGN" : "another");
    struct sigaction ignore = { .sa_handler = SIG_IGN, .sa_flags = SA_RESTART }, old = { .sa_handler = SIG_DFL };
    sigemptyset(&ignore.sa_mask);
    sigaddset(&ignore.sa_mask, SIGUSR1);
    int set = sigaction(SIGALRM, &ignore, NULL);
    int got = sigaction(SIGALRM, NULL, &old);
    printf("sigaction SIGALRM: %d %d, %s, flags 0x%lx, mask 0x%lx\n", set, got,
           old.sa_handler == SIG_IGN ? "SIG_IGN" : "another", old.sa_flags, (unsigned long)old.sa_mask);
    signal(SIGALRM, SIG_DFL);
    unsigned first = alarm(5);
    unsigned left = alarm(0);
    unsigned again = alarm(0);
    printf("alarm 5: %u; alarm 0: %s; again: %u\n", first, left >= 1 && left <= 5 ? "1 to 5" : "another", again);
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
    truncated();
    buffered();
    alarms();
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
    /* Volatile, so the compiler folds none of them. */
    volatile double halves[] = { 2.5, -2.5, 0.49999999999999994, 1e19 };
    printf("lround: %ld %ld %ld\n", lround(halves[0]), lround(halves[1]), lround(halves[2]));
    errno = 0;
    long far = lround(halves[3]);
    printf("lround(1e19): %ld, %s\n", far, errno == EDOM ? "EDOM" : errno_name(errno));
    printf("pathconf _PC_PATH_MAX: %ld\n", pathconf(DIR_PATH, _PC_PATH_MAX));
    printf("sysconf _SC_PAGE_SIZE: %ld\n", sysconf(_SC_PAGE_SIZE));
    return 0;
}
