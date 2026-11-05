# Building and running

## One command

From a clean clone, `cargo run` builds everything and boots the system in QEMU.
There is no Make, Docker or separately installed cross-toolchain: the build
system is an ordinary Rust program, and it names anything missing before it
starts. Rust and QEMU are the development prerequisites; the Rust compiler's
own bootstrap additionally needs a C compiler, Python and CMake on the host.
Linux and macOS are supported hosts.

## The toolchain is ToyOS's

ToyOS carries its own forks of the Rust compiler and of LLVM, in which the ToyOS
targets live. One LLVM is both rustc's code generator and the clang that builds
C programs, and its linker links the UEFI loader, the kernel and every program.
Nothing installed on the host goes into anything that boots. The first build
bootstraps that toolchain once per machine; later builds rebuild only what
changed.

## Images

Every build produces bootable disk images: GPT-partitioned, UEFI-only, and
ready to write to a USB stick. One kernel and loader serve three boot
configurations:

| Image | Purpose |
|---|---|
| The system | The whole desktop and its services; what QEMU boots |
| Diagnostic | Nothing may claim the display, so the kernel's log and panics stay readable on the panel of a machine with no serial port |
| Console | A shell on the raw framebuffer without the compositor |

Secure Boot must be off until the verified boot chain exists. The log partition
on the stick is FAT32, so any computer can read a boot's log afterwards.
Development images are signed with a key generated for the checkout; an image
for an owner's machine is signed with the owner's key.

## Tests

```text
   cheapest ────────────────────────────────────────────> most expensive
   types    host unit and module tests    QEMU guests    real machines
```

Decisions live in ordinary crates whose tests run on the host in milliseconds,
including models of the hardware. `cargo test` boots whole systems in QEMU for
behaviour that needs a guest. Timing, audio and hardware behaviour are judged on
real machines. QEMU establishes order, completion, contents and counts, never
timing.

## Reproducibility

Published OS contents are reproducible from pinned inputs, excluding
signatures and machine-specific installation data. Third-party sources are
pinned by hash or by commit.
