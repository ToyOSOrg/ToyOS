---
status: open
kind: defect
opened: 2026-09-29
---

# libc's `dlsym(RTLD_DEFAULT, …)` panics as unimplemented

`dlfcn.h` defines `RTLD_DEFAULT` as the null handle, and libc's `dlsym`
(`userland/libc/src/misc.rs`) panics on it: a handle is a module index plus
one, and `SYS_DLSYM` answers from one loaded library by index. Nothing searches
the global scope — the executable's exports, then every library in load order —
which is what glibc's `dlsym(RTLD_DEFAULT, …)` answers from. A C program that
probes the process for an optional symbol dies. The kernel-side sibling is
`issues/kernel/a-dlopened-library-never-binds-to-the-executables-exports.md`:
both want the executable inside the global scope.

**Exit**: `dlsym(RTLD_DEFAULT, name)` answers the first definition of `name` in
the executable and then every loaded library in load order, and NULL when there
is none, and a guest C case asserts both.
