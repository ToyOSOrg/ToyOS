---
status: open
kind: defect
opened: 2026-10-03
---

# Nothing launches a script by its interpreter line

A file that begins `#!` is not a program on ToyOS. A spawn of one is refused
`InvalidArgument` with every other file that is not an ELF
(`kernel/src/loader/mod.rs`, where `elf::parse_layout` fails), and nothing
above the kernel reads the line: `git grep -n -e '"#!"' -e ENOEXEC -- kernel
userland toyos toyos-abi` finds only `ENOEXEC`'s definition in
`userland/libc/include/errno.h`.

Owner: `issues/build/toyos-builds-itself.md`, whose tools (a POSIX `sh`, Perl,
make, CMake) are the first programs here that start a script by its path.

Exit condition: inside ToyOS, a spawn by path of an executable file whose
first line names an interpreter runs that interpreter on the file, shown by a
test that runs such a script and reads its exit code.
