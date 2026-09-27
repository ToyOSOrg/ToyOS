---
status: open
kind: defect
opened: 2026-09-27
---

# libc's C headers are written by hand, and nothing holds them to its definitions

`userland/libc/include/` is 32 headers typed beside the Rust that defines what
they declare, and no build or test compares the two. clang, compiling
doomgeneric for the first time, found two places they had already parted:

- `atof` was defined (`userland/libc/src/stdio.rs`) and declared nowhere, so
  `m_config.c`'s `(float) atof(value)` called it through C89's implicit `int`
  declaration — the value read out of `rax` for a function that returns in
  `xmm0`. toyos-cc compiled it that way without a word; clang refuses it.
- `usleep` was declared twice, `unsigned int` in `time.h` and `unsigned long` in
  `unistd.h`, against a definition taking a `u32`. toyos-cc took both.

Both are fixed in the headers. The class is not: a signature changed in
`userland/libc/src` changes no header, and the C sysroot
ships whatever the headers say. The corpus shows the
surface is also incomplete — `stdint.h` has no `least`/`fast` types, so clang's
own `stdatomic.h` does not compile (`124_atomic_counter`), and `pthread.h` and
`signal.h` stop short of `PTHREAD_PROCESS_SHARED` and `SIGUSR1`.

**Generating them with cbindgen was priced and not taken**: it is a new
dependency, and the headers are mostly what cbindgen cannot emit — macros,
constants, `FILE`, `va_list` plumbing — with the function prototypes the part
that drifts.

**Exit**: every function prototype in `userland/libc/include/` is checked
against the symbol and signature libc defines, by a test that reds on a
prototype with no definition, a definition a header was meant to declare and
does not, or two declarations of one name that disagree.
