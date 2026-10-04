/* What uname answers: the build ROOT carries, and no node name. The release
   is the commit's first twelve digits, with -dirty after them for a tree
   that was not that commit's; the version is the whole commit; the machine
   is the one this case was compiled for. */
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/utsname.h>

#if defined(__x86_64__)
#define MACHINE "x86_64"
#elif defined(__aarch64__)
#define MACHINE "aarch64"
#endif

/* Whether `s` is `n` lowercase hex digits and then `rest`. */
static int hex_then(const char *s, int n, const char *rest) {
    for (int i = 0; i < n; i++)
        if (!((s[i] >= '0' && s[i] <= '9') || (s[i] >= 'a' && s[i] <= 'f')))
            return 0;
    return strcmp(s + n, rest) == 0;
}

int main(void) {
    struct utsname uts;
    errno = 0;
    int answer = uname(&uts);
    printf("uname: %d, %s\n", answer, errno == 0 ? "no errno" : "an errno");
    if (answer != 0)
        return 1;
    printf("sysname: %s; nodename: %s\n", uts.sysname, uts.nodename[0] ? uts.nodename : "empty");
    printf("version: %s\n", hex_then(uts.version, 40, "") ? "a commit" : uts.version);
    printf("release: %s\n",
           (hex_then(uts.release, 12, "") || hex_then(uts.release, 12, "-dirty")) && strncmp(uts.release, uts.version, 12) == 0
               ? "the version's first twelve digits"
               : uts.release);
    printf("machine: %s\n", strcmp(uts.machine, MACHINE) == 0 ? "the one this case was compiled for" : uts.machine);
    return 0;
}
