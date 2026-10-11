/* dl_iterate_phdr visits the executable and every loaded library with the
   program headers the loader mapped, and stops where its callback says.
   Each module's addresses are checked against where its own header, code and
   _DYNAMIC really are, not against the kernel's other answers. */

#include <dlfcn.h>
#include <link.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

/* The image's test library; naming it is also what stages it beside this
   case. */
#define LIB "/system/lib/libtls_lib.so"

extern const char _DYNAMIC[] __attribute__((visibility("hidden")));
int main(void);

static int failed;

static void fail(const char *what, const char *name) {
    printf("FAIL %s: \"%s\"\n", what, name);
    failed = 1;
}

/* The PT_LOAD holding [addr, addr + len) at dlpi_addr + p_vaddr, or NULL. */
static const ElfW(Phdr) *load_holding(const struct dl_phdr_info *info, uintptr_t addr, size_t len) {
    for (int i = 0; i < info->dlpi_phnum; i++) {
        const ElfW(Phdr) *ph = &info->dlpi_phdr[i];
        uintptr_t lo = info->dlpi_addr + ph->p_vaddr;
        if (ph->p_type == PT_LOAD && addr >= lo && addr + len <= lo + ph->p_memsz)
            return ph;
    }
    return NULL;
}

/* The table and its count are the ones the module's own ELF header names:
   every module here is linked at vaddr 0 with its first PT_LOAD at file
   offset 0, so that header is at dlpi_addr. */
static void header_names_the_table(const struct dl_phdr_info *info, const char *name) {
    const unsigned char *ehdr = (const unsigned char *)info->dlpi_addr;
    if (memcmp(ehdr, "\177ELF", 4) != 0) {
        fail("no ELF header at dlpi_addr", name);
        return;
    }
    /* e_phoff and e_phnum, at their System V gABI offsets in Elf64_Ehdr. */
    uint64_t phoff;
    uint16_t phnum;
    memcpy(&phoff, ehdr + 32, sizeof phoff);
    memcpy(&phnum, ehdr + 56, sizeof phnum);
    if (info->dlpi_phnum != phnum)
        fail("dlpi_phnum is not e_phnum", name);
    if ((uintptr_t)info->dlpi_phdr != info->dlpi_addr + phoff)
        fail("dlpi_phdr is not dlpi_addr + e_phoff", name);
}

/* Whether `code` lies in an executable PT_LOAD of this module. */
static int runs(const struct dl_phdr_info *info, const void *code) {
    const ElfW(Phdr) *ph = load_holding(info, (uintptr_t)code, 1);
    return ph && (ph->p_flags & PF_X);
}

struct walk {
    int visited;
    int stop_at;
    int stop_with;
    void *lib_code;
};

static int visit(struct dl_phdr_info *info, size_t size, void *data) {
    struct walk *walk = data;
    walk->visited++;
    if (walk->stop_at)
        return walk->visited == walk->stop_at ? walk->stop_with : 0;

    const char *name = info->dlpi_name;
    if (size != sizeof(struct dl_phdr_info))
        fail("the size passed is not the record's", name);
    if (info->dlpi_phnum == 0 || info->dlpi_phdr == NULL) {
        fail("no program headers", name);
        return 0;
    }
    header_names_the_table(info, name);
    uintptr_t table = (uintptr_t)info->dlpi_phdr;
    if (!load_holding(info, table, info->dlpi_phnum * sizeof(ElfW(Phdr))))
        fail("the program headers lie in no PT_LOAD", name);
    for (int i = 0; i < info->dlpi_phnum; i++) {
        const ElfW(Phdr) *ph = &info->dlpi_phdr[i];
        if (ph->p_type == PT_PHDR && info->dlpi_addr + ph->p_vaddr != table)
            fail("PT_PHDR names another address", name);
    }

    if (walk->visited == 1) {
        if (strcmp(name, "") != 0)
            fail("the executable is not first, or is not named by the empty string", name);
        if (!runs(info, (const void *)main))
            fail("main lies in no executable PT_LOAD", name);
        int dynamic = 0;
        for (int i = 0; i < info->dlpi_phnum; i++) {
            const ElfW(Phdr) *ph = &info->dlpi_phdr[i];
            if (ph->p_type != PT_DYNAMIC)
                continue;
            dynamic = 1;
            if (info->dlpi_addr + ph->p_vaddr != (uintptr_t)_DYNAMIC)
                fail("PT_DYNAMIC is not at _DYNAMIC", name);
            if (!load_holding(info, info->dlpi_addr + ph->p_vaddr, ph->p_memsz))
                fail("PT_DYNAMIC lies in no PT_LOAD", name);
        }
        if (!dynamic)
            fail("no PT_DYNAMIC", name);
        printf("executable: \"%s\"\n", name);
    } else {
        if (strcmp(name, LIB) != 0)
            fail("a library other than the one loaded", name);
        if (!runs(info, walk->lib_code))
            fail("tls_get_label lies in no executable PT_LOAD", name);
        printf("library: %s\n", name);
    }
    return 0;
}

/* Walk every module, checking each; the count visited. */
static int walk_all(void *lib_code) {
    struct walk walk = { 0, 0, 0, lib_code };
    int ret = dl_iterate_phdr(visit, &walk);
    if (ret != 0) {
        printf("FAIL a walk no callback stopped returned %d\n", ret);
        failed = 1;
    }
    return walk.visited;
}

/* A walk the callback stops at module `at` with `with`. */
static void stop(int at, int with, int of) {
    struct walk walk = { 0, at, with, NULL };
    int ret = dl_iterate_phdr(visit, &walk);
    printf("stopped at %d of %d: visited %d, returned %d\n", at, of, walk.visited, ret);
    if (walk.visited != at || ret != with)
        failed = 1;
}

int main(void) {
    printf("modules: %d\n", walk_all(NULL));
    stop(1, 1, 1);

    void *lib = dlopen(LIB, RTLD_NOW);
    if (!lib) {
        printf("FAIL dlopen %s\n", LIB);
        return 1;
    }
    void *code = dlsym(lib, "tls_get_label");
    if (!code) {
        printf("FAIL dlsym tls_get_label\n");
        return 1;
    }
    printf("modules: %d\n", walk_all(code));
    stop(1, 7, 2);
    stop(2, -1, 2);
    return failed;
}
