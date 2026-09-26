---
status: open
kind: defect
opened: 2026-09-25
---

# Storage still comes up before init runs

ROOT is the loader's image in memory, and the kernel mounts it and spawns
init with no storage command issued (`root_from_memory` asserts the count
`rootfs::INIT_WITHOUT_A_DISK` logs is zero). But `kernel_main` still brings
up NVMe, xHCI, `fat32_adapter::probe_boot_disks` and the DATA, `/boot` and
`/log` mounts in boot context after that spawn and before `smp::set_ready`
releases the machine, so every one of them runs before init's first
instruction. The track's stage 1 exit
(`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`)
asks for them to be untouched until init runs.

Two things hold it there. `xhci::init` binds a USB stick's mass storage —
INQUIRY, READ CAPACITY — inside the one enumeration scan that runs before
there is a scheduler, so a USB-boot machine issues storage commands in it
whatever order the rest takes; and nothing in the VFS makes a syscall on
`/home`, `/apps`, `/log` or `/boot` wait for a mount that has not happened
yet, so moving the mounts past the release would hand logd and the shell a
missing directory instead of a late one.

Measured on QEMU 11 TCG, USB-stick boot of the `--build-only` image: `Boot:
storage ready` is 118 ms of a 222 ms kernel boot, all of it now after init's
spawn and before init runs.

Exit: a boot whose storage drivers first speak after init has run, with the
USB bind out of the pre-scheduler scan (the track's stage 5) and the
storage mounts awaited rather than absent.
