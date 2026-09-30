---
status: open
kind: defect
opened: 2026-09-27
---

# ToyOS has no C compiler inside it

Until 2026-09-27 every image carried `/system/bin/toyos-cc`, a C compiler that
ran on ToyOS, beside `/system/bin/toyos-ld`. clang replaced toyos-cc on the host
and toyos-cc was deleted with its `system.toml` row, so a C program can be
compiled *for* ToyOS and not *on* it. Nothing in the suite ran the in-guest
compiler, so no test went with it.

`toyos-ld` is still in the image, frozen, and links objects a ToyOS process
already has; nothing in the image makes one from C.

Before any package, LLVM does not build for a ToyOS host, and so neither do
clang and lld: `issues/build/the-c-sysroot-has-no-libm-so-llvms-configure-fails.md`,
`issues/build/llvm-and-rustcs-build-have-no-arm-for-a-toyos-host.md`,
`issues/build/toyos-libc-lacks-the-posix-surface-llvm-compiles-against.md` and
`issues/build/libc-has-no-alarm.md`; and one that built would read and write
files wrongly through
`issues/build/libc-mmap-ignores-the-file-it-is-asked-to-map.md`,
`issues/build/libc-fcntl-and-fchmod-answer-0-and-do-nothing.md`,
`issues/build/libc-pread-and-pwrite-move-the-offset-another-thread-shares.md`
and `issues/build/libc-readdir-calls-every-entry-a-regular-file.md`. The
self-hosting track orders them.

**Exit**: the self-hosting track's M2 (`issues/build/toyos-builds-itself.md`) —
clang and lld installed as a package, and a guest test that compiles `hello.c`
inside ToyOS and runs what it built.
