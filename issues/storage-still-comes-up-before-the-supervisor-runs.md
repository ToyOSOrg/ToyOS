---
status: open
kind: defect
opened: 2026-09-25
---

# Storage still comes up before init runs

ROOT is the loader's image in memory, and the kernel mounts it and spawns
init with no storage command issued (`root_from_memory` asserts the count
`rootfs::INIT_WITHOUT_A_DISK` logs is zero). But `kernel_main` still brings
up xHCI and reads every USB disk's table (`gpt::probe_usb_disks`) in boot
context after that spawn and before `smp::set_ready` releases the machine, so
both run before init's first instruction. The track's stage 1 exit
(`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`)
asks for them to be untouched until init runs.

What holds it there: `xhci::init` binds a USB stick's mass storage —
INQUIRY, READ CAPACITY — inside the one enumeration scan that runs before
there is a scheduler, so a USB-boot machine issues storage commands in it
whatever order the rest takes. The volumes are no longer the kernel's: the
file servers mount them once init starts them.

Measured on QEMU 11 TCG, USB-stick boot of the `--build-only` image: `Boot:
storage ready` is 118 ms of a 222 ms kernel boot, all of it now after init's
spawn and before init runs.

Exit: a boot whose storage drivers first speak after init has run, with the
USB bind out of the pre-scheduler scan (the track's stage 5).
