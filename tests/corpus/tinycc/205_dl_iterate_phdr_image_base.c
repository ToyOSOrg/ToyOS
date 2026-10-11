/* dl_iterate_phdr reports the executable's program headers where they are when
   its lowest address is not 0: tests/common/compile.rs links this case at
   --image-base=0x200000, so the load bias and the image's first byte differ.
   Every fact the answer is checked against is the linker's, reached through
   __ehdr_start and never through the kernel's answer. */

#include <link.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

extern const unsigned char __ehdr_start[] __attribute__((visibility("hidden")));

/* The executable is visited first; keep its record and stop. */
static int first(struct dl_phdr_info *info, size_t size, void *data) {
    (void)size;
    *(struct dl_phdr_info *)data = *info;
    return 1;
}

static int failed;

static void check(int holds, const char *what) {
    printf("%s: %s\n", what, holds ? "yes" : "no");
    if (!holds)
        failed = 1;
}

int main(void) {
    struct dl_phdr_info exe;
    if (dl_iterate_phdr(first, &exe) != 1) {
        printf("FAIL the walk did not stop at the executable\n");
        return 1;
    }

    /* e_phoff and e_phnum, at their System V gABI offsets in Elf64_Ehdr. */
    uint64_t phoff;
    uint16_t phnum;
    memcpy(&phoff, __ehdr_start + 32, sizeof phoff);
    memcpy(&phnum, __ehdr_start + 56, sizeof phnum);
    const ElfW(Phdr) *table = (const ElfW(Phdr) *)(__ehdr_start + phoff);

    uintptr_t lowest = UINTPTR_MAX;
    for (int i = 0; i < phnum; i++)
        if (table[i].p_type == PT_LOAD && table[i].p_vaddr < lowest)
            lowest = table[i].p_vaddr;
    printf("lowest PT_LOAD: 0x%lx\n", (unsigned long)lowest);

    check(exe.dlpi_phnum == phnum, "dlpi_phnum is e_phnum");
    check(exe.dlpi_phdr == table, "dlpi_phdr is __ehdr_start + e_phoff");
    check((uintptr_t)__ehdr_start == exe.dlpi_addr + lowest, "__ehdr_start is dlpi_addr + the lowest p_vaddr");
    return failed;
}
