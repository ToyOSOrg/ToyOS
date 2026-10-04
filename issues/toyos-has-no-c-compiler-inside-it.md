---
status: open
kind: defect
opened: 2026-09-27
---

# ToyOS has no C compiler inside it

Until 2026-09-27 every image carried `/system/bin/toyos-cc`, a C compiler that
ran on ToyOS. clang replaced toyos-cc on the host
and toyos-cc was deleted with its `system.toml` row, so a C program can be
compiled *for* ToyOS and not *on* it. Nothing in the suite ran the in-guest
compiler, so no test went with it.

Nothing in the image makes an object from C, nor links one.

**Exit**: the self-hosting track's M2 (`issues/toyos-builds-itself.md`) —
clang and lld installed as a package, and a guest test that compiles `hello.c`
inside ToyOS and runs what it built.
