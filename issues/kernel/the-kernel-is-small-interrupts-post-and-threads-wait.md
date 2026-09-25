---
status: open
kind: track
opened: 2026-09-25
---

# The kernel is small: interrupts post, threads wait, and storage lives in userland

Owner ruling, 2026-09-25: the kernel is to be super small, super performant
and safe. This track holds that, and supersedes the ordering of
`issues/kernel/every-wait-in-this-kernel-is-a-spin.md`,
`issues/kernel/every-driver-is-still-in-the-kernel.md` and
`issues/hardware/the-bot-scsi-machine-is-still-hand-written-in-the-kernel.md`,
which stay as the evidence each stage closes.

The design review of 2026-09-25 read the code and found the same shape three
times:
- **Four wait mechanisms stacked** where one would do: the scheduler's
  multi-waiter queue used only as a one-thread park, the completion record ring
  whose records every caller discards, the userland poll registry's closed
  `Source` enum with two hand-written dispatches, and ad-hoc wake paths.
- **USB with three concurrency models:** a state machine inside the scheduler
  pass, disk I/O spinning with interrupts off for up to 4.75 s under one global
  lock, and boot discovery calling a blocking bind from a scheduler pass. The
  thread reserved to own the controller, `usbd`, only parks.
- **Storage done busy-waiting under spinlocks:** one global VFS lock, NVMe with
  one command outstanding and polled, and a 2 s operation budget "with
  preemption off" that budgets audio stalls. That budget drags a refusal chain
  through five layers (deadline slots, retries and backoff, `BudgetExpired`,
  writeback re-enqueue, the `MID_UPDATE` task bit).

## Stages

1. **The system image is loaded into memory by the loader.** The UEFI loader
   reads the selected slot's ROOT through the firmware's own block I/O, checks
   its signature on the bytes in memory (the self-update's signing, so one
   check covers both), and hands the kernel its address and length. The kernel
   mounts ROOT from memory and needs no storage driver to boot.
   **Exit**: the machine boots in QEMU and on the T14 with the kernel's NVMe and
   USB storage paths never touched before init runs. The boot-time cost on the
   stick and on NVMe is measured.
2. **One way to wait.** Every waitable object has one `Watch`, and a waiter is
   either a thread (woken) or a user poll ring (posted). The multi-waiter queue,
   the per-thread record ring, the `Source` double dispatch and the per-module
   watcher lists are deleted. **Exit**: making an object waitable is one field,
   and the interleaving models check the smaller protocol.
3. **Storage in userland.** NVMe and USB mass storage are programs that claim
   their device through its IOMMU domain, as netd does, and serve a block
   protocol over shared-memory rings. Partition claims move into that service.
   The kernel's NVMe driver, USB storage, block layer, page cache and the whole
   budget and refusal chain are deleted. **Exit**: a crashed disk service
   restarts without the kernel noticing, `/log` and `/home` survive it, and
   NVMe keeps more than one command in flight.
4. **Where the data filesystem lives** (kernel VFS over a userland block
   service, or a userland file server) is decided with the owner from a roasted
   proposal before it is built.
5. **USB owned by its thread, then by userland**, with discovery and recovery
   written once as straight-line code. **Exit**: no interrupts-off window
   longer than a register access, and keyboard input keeps flowing while a
   stick misbehaves.
6. **The scheduler knows nothing about devices.** Interrupt handlers only post
   to their device's `Watch`, and the device's thread does the work. The
   per-CPU IRQ relay, the driver list in the scheduler pass and the idle
   special cases are deleted.

## Standing

No device wait, spin or poll happens while holding a spinlock or inside a
scheduler pass; interrupts post, threads wait. Every stage measures the kernel
it leaves behind (lines, the longest interrupts-off window, the longest
preemption-off window) against the one it started from.
