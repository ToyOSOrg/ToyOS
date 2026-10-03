---
status: open
kind: track
opened: 2026-09-25
---

# The kernel is small: interrupts post, threads wait, and storage lives in userland

Owner ruling, 2026-09-25: the kernel is to be super small, super performant
and safe. This track holds that, and supersedes the ordering of
`issues/kernel/every-driver-is-still-in-the-kernel.md` and
`issues/hardware/a-disk-plugged-in-after-boot-is-bound-inside-a-scheduling-pass.md`,
which stay as the evidence each stage closes.

The design review of 2026-09-25 read the code and found the same shape three
times:
- **Four wait mechanisms stacked** where one would do: the scheduler's
  multi-waiter queue used only as a one-thread park, the completion record ring
  whose records every caller discards, the userland poll registry's closed
  `Source` enum with two hand-written dispatches, and ad-hoc wake paths.
- **USB with three concurrency models:** a state machine inside the scheduler
  pass, disk I/O spinning with interrupts off for up to 4.75 s under one global
  lock, and boot discovery calling a blocking bind from a scheduler pass.
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
   measured yet: ToyOS is not installed on the T14's NVMe, so there is no ROOT
   there to read until the self-update track installs it
   (`issues/boot-media/the-machine-updates-itself-without-ubuntu.md`).
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
      `SYS_PARTITION_READ/WRITE` deleted once no disk the kernel drives serves
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
      block layer, GPT, the NVMe driver, the refusal chain and the file
      syscalls. **Exit**: `BudgetExpired`, `DEADMAN` and
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
       the dump with usbd killed. Where QEMU's and the T14's xHCI keep their
       MSI-X tables is not measured: one in the BAR that holds the registers
       refuses usbd's claim as it refuses blockd's
       (`issues/kernel/a-controller-whose-msix-table-is-in-bar-0-cannot-be-driven-from-userland.md`).
5. **USB by userland**, with discovery and recovery
   written once as straight-line code. **Exit**: no interrupts-off window
   longer than a register access, and keyboard input keeps flowing while a
   stick misbehaves.
6. A handler posts its
   device's `Watch` and ends its interrupt; the thread waiting on that watch
   does the work, and no step creates a kernel thread. `irq_ring`, the driver
   list in `drain_irqs` and the idle loop's device checks are gone by step 5.
   Each step measures the kernel's lines against stage 6's first commit, and
   the longest interrupts-off and preemption-off windows against the readings
   step 2's exit puts in the tree, by that exit's rule.
   Steps 3 and 4 do not land alone: they land with #592's i8042 stage and
   with usbd.
   1. **Interrupts post.** A post is legal in a handler: the watches a handler
      posts, and the completions of a ring they complete into, sit behind
      interrupts-off locks nothing allocates or frees under, and every other
      watch's lock leaves interrupts open. A claimed function's vector, the
      IOMMU's refusal and both audio backends post from the handler, and
      `irq_ring`'s `UserDev` and `Audio` and their arms in `drain_irqs` go.
      The thread is the holder's: netd's, blockd's, soundd's mix thread, and
      the `isa` claim's when #592 lands. **Exit**: `handler_post_without_a_pass`,
      a vector taken on a CPU holding preemption off, inside a post of its own
      watch, inside a completion into a ring polling it, or inside that ring's
      own watch, posting once that section lets go and before any pass, red on
      the base; the watch's loom models over the new post.
   2. **The windows, measured**: the longest interrupts-off and preemption-off
      windows per CPU, reported beside the IRQ census and fed by each
      architecture's masking primitives and entries, the number the ARM
      track's stage 4 owes as well. Built: the `mask-windows` kernel and the
      T14's `mask_windows` row. The exit's first half is met: the instrument
      reads back a window of known length on the T14, which is x86 metal, and
      the row's judge refuses a boot that reads it back shorter than it was
      held or at more than twice that. **Exit**, open: neither window is
      longer with step 1 than
      with it reverted on the tree that carries the instrument, the patch
      posted on #649 being what reverted means. That comparison is the median
      of at least five interleaved boots an arm, each read from the load's own
      report with a report taken as the load starts, against a tolerance
      stated before measuring, the reverted arm's own spread, with the tail
      reported beside it, under a load in which a handler step 1 changed posts
      into a watch that reaches a ring with parked threads
      (`issues/kernel/a-process-lengthens-an-interrupts-off-walk-by-the-threads-it-parks-on-one-ring.md`'s
      second bullet). `ring_park_herd` is not that load: its walks start in
      `SYS_INBOX_SUBMIT`, which runs with interrupts masked on both arms, and
      it reaches no handler step 1 changed.
   3. **The i8042's thread is ps2server's** (#592's i8042 stage): `irq_ring`'s
      `I8042`, `keyboard_controller::service` and the idle loop's
      `verdict_due` go with the kernel's driver. **Exit**: that stage's.
   4. **xHCI's thread is usbd's** (step 10 above): `Xhci`, `poll_if_pending`
      and `port_work_pending` go with the kernel's driver, and `irq_ring` with
      them. **Exit**: step 10's.
   5. **The pass is the scheduler's.** `drain_irqs` goes: the blocked-task
      dump and the heartbeat become `pass`'s own, and the TCO feed stays,
      since what it proves is that passes run. The dump keeps painting its
      report on the panel and holding it there, a device the pass reaches
      (owner, 2026-09-30). **Exit**: `drain_irqs` and the
      idle loop's device checks are gone, both windows are measured against
      step 2's readings by its rule, and the exits of
      `issues/kernel/an-irq-watchs-freeing-cancel-compiles-in-a-handler.md`
      and
      `issues/kernel/nothing-fails-when-a-devices-release-or-close-stops-answering-its-polls.md`
      are met.

## Standing

No device wait, spin or poll happens while holding a spinlock or inside a
scheduler pass; interrupts post, threads wait. Every stage measures the kernel
it leaves behind (lines, the longest interrupts-off window, the longest
preemption-off window) against the one it started from.
