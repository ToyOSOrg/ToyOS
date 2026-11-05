# ToyOS

ToyOS is a general-purpose operating system written from scratch in Rust: a
desktop and development system its authors use every day and can change
quickly, held to a production bar of correct, efficient and minimal.

This manual is the intended design: what the system must deliver and how it
works or is meant to work. It does not track progress; where the system does
not yet match a page, the difference is staged work in the project's issues.
It names no source files and quotes no code, so it cannot drift when code
moves. A change to the design or a requirement updates its page in the same
change.

[Overview](architecture/overview.md) is the shortest way in.

## Targets

- Modern x86-64 (2020 and later) and AArch64, as equal targets.
- UEFI only: no BIOS, no 32-bit code, no backwards compatibility.
- Real machines are first-class; QEMU is supported.

## The north star

ToyOS rebuilds itself on its own hardware and reproduces, byte for byte, what
the host built.
