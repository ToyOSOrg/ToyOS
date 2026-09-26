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
   **Done** (#506; T14 run 143 green): on the stick the loader reads ROOT in
   1160 ms, the kernel reaches `Boot: complete` 1229 ms after it starts, and
   the kernel issues 0 storage commands before init. The NVMe cost is not
   measured and cannot be on the T14: nothing is ever written to its NVMe, so
   there is no ROOT there to read.
2. **One way to wait.** Every waitable object has one `Watch`, and a waiter is
   either a thread (woken) or a user poll ring (posted). The multi-waiter queue,
   the per-thread record ring, the `Source` double dispatch and the per-module
   watcher lists are deleted. **Exit**: making an object waitable is one field,
   and the interleaving models check the smaller protocol.
   **Done** (#513): `toyos-sched/src/watch.rs` and `park.rs`. `kernel/src` is
   720 lines smaller (62931 to 62211). No instrument in the tree measures an
   interrupts-off or a preemption-off span. The nearest, `sched_check_build`'s
   pass cost, separated nothing on the dev host: the largest pass per CPU was
   2.5–3.3 ms on this stage and 2.6–5.0 ms on main, three interleaved runs each.
3. **Storage in userland.** NVMe and USB mass storage are programs that claim
   their device through its IOMMU domain, as netd does, and serve a block
   protocol over shared-memory rings. Partition claims move into that service.
   The kernel's NVMe driver, USB storage, block layer, page cache and the whole
   budget and refusal chain are deleted. **Exit**: a crashed disk service
   restarts without the kernel noticing, `/log` and `/home` survive it, and
   NVMe keeps more than one command in flight. Met on QEMU at step 6 below.
   **On the T14 it is met only when usbd lands** (step 10): the T14's only
   writable storage is its stick, and until then that is the kernel's.
4. **Where the data filesystem lives** — owner ruling, 2026-09-26, from the
   roasted proposal: userland file servers, the VFS a client library, and a
   program's view a set of directory capabilities init hands it, each resolved
   by its server. The page cache lives in the file server. One file server per
   role (LOG, DATA, BOOT), so one crashing cannot take another down. A
   file-server crash is survivable, not invisible: data survives on disk,
   clients reopen, and the few states that cannot be recovered answer `Gone`;
   invisible restart can come later. The kernel keeps ROOT's in-memory read
   path and exec from it. USB storage stays in the kernel until stage 5 moves
   the whole xHCI out, because its one IOMMU domain is shared with the
   keyboard, and the panic console and the kernel's hotkeys never depend on a
   userland USB program. No swap, declared. `SYS_DEVICE_DMA_MAP` for zero-copy
   block I/O. FAT32 on `/log` in the installed product is deferred.

   Stages 3 and 4 are built as these steps, one pull request each:
   1. **`toyos-blockring`**, the protocol, pure. **Exit**: an interleaving
      model of submit, complete, reset, crash and reconnect, red under a lost
      completion, a double completion, and a loss nothing is written again
      after. **Done** (#525).
   2. **`SYS_DEVICE_DMA_MAP`**. **Exit**: a claimed function reaches a lent
      region and nothing else, and a transfer past it or after it is taken back
      is a `DMA FAULT` record. **Done** (#525).
   3. **blockd**, NVMe in userland beside the kernel's driver. **Exit**: more
      than one command in flight across several queues, measured; Flush issued
      when the controller has a volatile write cache; killed with a write on
      the device and restarted, what was acknowledged survives, what was not is
      refused, and the volume checks clean; init restarts it. **Built** (#525)
      but for the last: init closes the ports of a service that ends, so the
      restart is a supervisor's in the test.
   4. **Partitions in blockd**. **Exit**: a partition held by one session at a
      time, the idle slot claimed by GUID and written through blockd, and
      `SYS_PARTITION_READ/WRITE` retired once no disk the kernel drives serves
      a claim. **Built** (#525) but for the last: the kernel's disks, the stick
      among them, still serve claims until step 10, and a `part:` row still
      mints a kernel claim.
   5. **Spawn and dlopen from an image handle.** **Exit**: a program in `/apps`
      runs having been read by userland, its faults served from the image
      object and not the VFS.
   6. **fsd for DATA over blockd**, with the `toyos::fs` client the std fork
      uses; the kernel's DATA mount deleted with it. **Exit**:
      `home_overwrite_reads_back` and `apps_and_home_are_one_filesystem`
      green; fsd killed mid-write, restarted, the volume mounts, fsync'd data
      reads back on the host, and the kernel log does not change.
   7. **fsd for LOG and BOOT; logd on the client library.** On the T14 the
      stick is still the kernel's, so this step builds **the kernel USB
      bridge**: the kernel's USB mass storage serves `toyos-blockring`
      sessions, one per partition held, as blockd does, and fsd opens LOG
      through it. It is the one block server the kernel keeps, a compromise
      with an exit of its own: deleted at step 10, when usbd serves the same
      sessions. **Exit**: the log's durability tests without `WouldBlock`, on
      QEMU over blockd and on the T14 over the bridge; the quiesce cases green
      with the storage chain kept running; and the panic wait still publishes
      the last line.
   8. **Views as capabilities**, the isolation track's stage 1. **Exit**: that
      stage's escape suite, run against fsd.
   9. **Delete the kernel storage stack**: the VFS down to ROOT's resolver,
      both writable adapters, both caches, write-back, durability, tmpfs, the
      block layer, GPT, the NVMe driver, the refusal chain and the retired file
      syscalls' numbers. **Exit**: `BudgetExpired`, `DEADMAN` and
      `between_attempts` appear nowhere, and the kernel's lines and longest
      interrupts-off and preemption-off windows are measured.
   10. **usbd**, stage 5's second half: the whole xHCI moves, HID to the
       keyboard claim and mass storage over `toyos-blockring`, and the kernel
       USB bridge is deleted. **What must work with no userland stays off
       USB**: the panic console pages its report by itself and needs no
       keyboard, and steers only from the i8042 it polls; the kernel's one
       hotkey, Ctrl+Alt+D (`kernel/src/keyboard.rs`, the blocked-task dump),
       is recognised on the i8042's transitions and no longer on a USB
       keyboard's, which from here reach the kernel only as usbd's keyboard
       claim and are never read for it. A machine whose only keyboard is USB
       has no kernel hotkey from this step, declared. **Exit**, on the T14:
       `/log` survives usbd killed mid-batch, the keyboard keeps working while
       a stick misbehaves, and Ctrl+Alt+D on the machine's own keyboard files
       the dump with usbd killed.
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
