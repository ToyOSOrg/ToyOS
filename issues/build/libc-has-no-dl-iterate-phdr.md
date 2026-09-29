---
status: open
kind: track
opened: 2026-09-28
---

# libc has no `dl_iterate_phdr`

libunwind finds a module's unwind tables through `dl_iterate_phdr`, so M3 of
`issues/build/toyos-builds-itself.md` needs it in `userland/libc`. It is not a
libc change alone: the process starts with `argc` and `argv` on its stack and no
auxiliary vector, and `SYS_QUERY_MODULES`'s `ModuleInfo` carries a module's
base, extent and `.eh_frame_hdr` but not where its program headers are, so
nothing tells userland a loaded module's `PT_*` table. Reading them off the
module's base assumes a mapped ELF header, which no loader code promises.

**Exit**: `dl_iterate_phdr` in libc and `link.h` in its headers, visiting the
executable and every loaded library with its program headers, from an answer
the kernel gives rather than one assumed.
