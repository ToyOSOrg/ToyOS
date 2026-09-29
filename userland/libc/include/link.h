#ifndef _LINK_H
#define _LINK_H

#include <elf.h>
#include <stddef.h>

#define ElfW(type) Elf64_##type

/* The first four fields of glibc's layout. There is no dlpi_adds or
   dlpi_subs, so the size a callback is passed ends at dlpi_phnum.
   dlpi_name is "" for the executable, and lives only until the callback
   returns. */
struct dl_phdr_info {
    ElfW(Addr) dlpi_addr;
    const char *dlpi_name;
    const ElfW(Phdr) *dlpi_phdr;
    ElfW(Half) dlpi_phnum;
};

int dl_iterate_phdr(int (*callback)(struct dl_phdr_info *info, size_t size, void *data), void *data);

#endif
