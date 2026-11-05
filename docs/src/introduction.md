# ToyOS

ToyOS is a general-purpose operating system written from scratch in Rust: a
desktop and development system that its authors use every day and can change
quickly. It is held to a production bar: correct, efficient and minimal.

This manual describes the intended design: what the system must deliver and how
it works or is meant to work. It is the place to read when you want to know why
something is shaped the way it is. It does not track progress; where the system
does not yet match a page, the difference is staged work recorded with the
project's issues.

The manual names no source files and quotes no code, so that it cannot drift
when code moves. A change to the design or to a requirement updates the page
that states it, in the same change.

## In one paragraph

A small kernel protects memory, schedules threads, routes interrupts, isolates
device DMA and moves capabilities between processes. Everything else, including
drivers, filesystems, networking, audio and the desktop, runs as ordinary
userland processes that hold only what they were given. A program reaches a
file, a device or a service only through a capability that someone with the
authority to do so handed it. Ordinary Rust programs build for ToyOS unchanged,
and the same applications run on ToyOS, Linux under Wayland, macOS and Windows.

## Targets

Modern x86-64 (2020 and later) and AArch64 are equal targets, booted only
through UEFI. Real machines are first-class and QEMU is supported. There is no
BIOS, no 32-bit code and no backwards compatibility.

## The north star

ToyOS must eventually rebuild itself on its own hardware and reproduce,
byte for byte, what the host built.
