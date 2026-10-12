---
status: open
kind: track
opened: 2026-09-25
---

# The kernel is small: interrupts post, threads wait, and storage lives in userland

Owner ruling, 2026-09-25: the kernel is to be super small, super performant
and safe. This track holds that, and supersedes the ordering of
`issues/every-driver-is-still-in-the-kernel.md` and
`issues/a-disk-plugged-in-after-boot-is-bound-inside-a-scheduling-pass.md`,
which stay as the evidence each stage closes. Owner: the orchestrator, which
dispatches each step.

## What is left, in order

1. **ps2server**: stage 7 step 2, with stage 6 step 3.
2. **Step 9's file half** (stage 4), once libc is a file server's client
   (`issues/c-programs-name-no-file-a-file-server-holds.md`).
3. **Step 8's exit** (stage 4). Its empty view needs step 9's file half:
   until then the kernel serves `/system` and `/tmp` to every process.
4. **The pcidev MSI ruling** (step 10).
5. **usbd's whole xHCI**: step 10, with stage 6 step 4, step 4's tail and
   step 9's block half, which serve the stick until usbd does.
6. **Stage 6 step 5, then stage 5's windows.**

Beside that order: stage 6 step 2's comparison, which needs only its load,
and step 5's dlopen, which waits on the same libc client as step 9's file
half.

## Stages

1. **The system image is loaded into memory by the loader.** The UEFI loader
   reads the selected slot's ROOT through the firmware's own block I/O, checks
   its signature on the bytes in memory (the self-update's signing, so one
   check covers both), and hands the kernel its address and length. The kernel
   mounts ROOT from memory and needs no storage driver to boot.
   **Exit**: the machine boots in QEMU and on the T14 with the kernel's NVMe
   and USB storage paths never touched before the supervisor runs. The
   boot-time cost on the stick and on NVMe is measured.
   **Partly done** (#506; T14 run 143 green): on the stick the loader reads
   ROOT in 1160 ms, the kernel reaches `Boot: complete` 1229 ms after it
   starts, and the kernel issues 0 storage commands before it spawns the
   supervisor. **Left**: `kernel_main` runs `xhci::init` and
   `gpt::probe_usb_disks` after that spawn and before `smp::set_ready`, so
   before the supervisor's first instruction
   (`issues/storage-still-comes-up-before-the-supervisor-runs.md`); both go
   with the kernel's xHCI at step 10. The NVMe cost waits on a ROOT on the
   T14's NVMe, which the self-update track installs
   (`issues/the-machine-updates-itself-without-ubuntu.md`).
2. **One way to wait.** Every waitable object has one `Watch`, and a waiter is
   either a thread (woken) or a user poll ring (posted). The multi-waiter
   queue, the per-thread record ring, the `Source` double dispatch and the
   per-module watcher lists are deleted. **Exit**: making an object waitable
   is one field, and the interleaving models check the smaller protocol.
   **Done** (#513): `kernel/pure/sched/watch.rs` and `park.rs`; `kernel/src`
   went from 62931 to 62211 lines.
3. **Storage in userland.** NVMe and USB mass storage are programs that claim
   their device through its IOMMU domain, as netstack does, and serve a block
   protocol over shared-memory rings. Partition claims move into that service.
   The kernel's NVMe driver, USB storage, block layer, page cache and the
   whole budget and refusal chain are deleted. **Exit**: a crashed disk
   service restarts without the kernel noticing, `/log` and `/home` survive
   it, and NVMe keeps more than one command in flight.
   **Partly done**: met on QEMU, where diskserver drives NVMe (#525, #536),
   and the kernel's NVMe driver is gone (#536). **Left**: on the T14, `/log`
   and `/home` are on the stick, which the kernel drives until step 10; the
   kernel's USB storage, block layer, page cache and refusal chain go at
   step 9.
4. **Where the data filesystem lives** — owner ruling, 2026-09-26, from the
   roasted proposal: userland file servers, the VFS a client library, and a
   program's view a set of directory capabilities the supervisor hands it,
   each resolved by its server. The page cache lives in the file server. One
   file server per role (LOG, DATA, BOOT), so one crashing cannot take another
   down. A file-server crash is survivable, not invisible: data survives on
   disk, clients reopen, and the few states that cannot be recovered answer
   `Gone`; invisible restart can come later. The kernel keeps ROOT's
   in-memory read path and exec from it. USB storage stays in the kernel until
   stage 5 moves the whole xHCI out, because its one IOMMU domain is shared
   with the keyboard, and the panic console never depends on a userland USB
   program. Ctrl+Alt+D is removed (owner, 2026-10-10: "i dont want it. i want
   it removed."; done, #824). No swap, declared. `SYS_DEVICE_DMA_MAP` for
   zero-copy block I/O. FAT32 on `/log` in the installed product is deferred.
   **Partly done**: steps 1, 2, 3, 6 and 7 are done, and 4, 5, 8, 9 and 10
   partly.

   Stages 3 and 4 are built as these steps:
   1. **`toyos-blockring`**, the protocol, pure. **Exit**: an interleaving
      model of submit, complete, reset, crash and reconnect, red under a lost
      completion, a double completion, and a loss nothing is written again
      after. **Done** (#525).
   2. **`SYS_DEVICE_DMA_MAP`**. **Exit**: a claimed function reaches a lent
      region and nothing else, and a transfer past it or after it is taken
      back is a `DMA FAULT` record. **Done** (#525).
   3. **diskserver**, NVMe in userland. **Exit**: more than one command in
      flight across several queues, measured; Flush issued when the
      controller has a volatile write cache; killed with a write on the
      device and restarted, what was acknowledged survives, what was not is
      refused, and the volume checks clean; the supervisor restarts it.
      **Done** (#525; the restart, #536).
   4. **Partitions in diskserver**. **Exit**: a partition held by one session
      at a time, the idle slot claimed by GUID and written through
      diskserver, and `SYS_PARTITION_READ/WRITE` deleted once no disk the
      kernel drives serves a claim. **Partly done** (#525, #826): sessions, a
      block connector that opens only the partitions its badge grants, and
      the idle slot on diskserver's disk written through one. **Left**: on a
      disk the kernel drives, the stick among them, the supervisor still
      claims a role's and the idle slot's partitions from the kernel, and
      `SYS_PARTITION_READ/WRITE` serve them; both go with the kernel's USB
      storage at step 10.
   5. **Spawn and dlopen from an image handle.** **Exit**: a program in
      `/apps` runs having been read by userland, its faults served from the
      image object and not the VFS. **Partly done** (#536): spawn
      (`SpawnArgs::image`). **Left**: dlopen. `SYS_DLOPEN` opens a path the
      kernel serves, so no library on a file server loads
      (`issues/a-package-cannot-ship-its-own-libraries.md`); #536 left
      dlopen from an image out of the ABI, since nothing calls it before libc
      is a file server's client. Its exit: a library in `/apps` loads having been read
      by userland, its faults served from the image object.
   6. **fileserver for DATA over diskserver**, with the `toyos::fs` client the
      std fork uses; the kernel's DATA mount deleted with it. **Exit**:
      `home_overwrite_reads_back` and `apps_and_home_are_one_filesystem`
      green; fileserver killed mid-write, restarted, the volume mounts,
      fsync'd data reads back on the host, and the kernel log does not
      change. **Done** (#536). #660 cut both guest tests, and under the
      owner's testing ruling (#890: "we try not to test integration between
      modules unless its the kernel") neither comes back: each started
      userland programs, drove them through writes and a shutdown, and read
      the disk on the host, so what they checked is fileserver's and
      `bcachefs`'s to hold in module tests. The guest-suite track's stage D
      still lists both.
   7. **fileserver for LOG and BOOT; logkeeper on the client library.**
      **Exit**: the log's durability tests without `WouldBlock`, the quiesce
      cases green with the storage chain kept running, and the panic wait
      still publishes the last line. **Done** (#536), without the kernel USB
      bridge this step had planned: on a disk the kernel drives, fileserver
      opens LOG and BOOT through the kernel's partition claims, which step 10
      deletes with the kernel's USB storage.
   8. **Views as capabilities**, the isolation track's stage 1
      (`issues/every-program-sees-only-the-files-it-was-given.md`).
      **Exit**: that stage's, run against fileserver: an escape suite red on
      a resolver that walks the global tree and green on fileserver's, and a
      process given an empty view naming nothing. **Partly done** (#729,
      #742): a connection is the grant the supervisor minted on its role's
      one port, and `fs_escape` is refused a symlink that climbs out, a `..`
      on the wire and a directory it was not given. **Left**: the rest of the
      escape suite, a rename racing a lookup among it, shown red on a
      global-tree resolver; and the empty view, which names `/system` and
      `/tmp` while the kernel serves them (step 9's file half).
   9. **Delete the kernel storage stack.** **Partly done** (#536): the NVMe
      driver, both writable adapters, write-back, durability and the
      `MID_UPDATE` bit are gone. Two halves are left, and they land apart:
      - **The file half**: the VFS down to ROOT's resolver, tmpfs (`tmpfs.rs`,
        `fs_rename.rs`), the page cache (`file_cache.rs`) and the file
        syscalls. `/tmp` leaves the kernel per session, as the owner ruled
        ("Per session", 2026-10-03, in the isolation track), and `/system` is
        read over the file protocol, which
        `issues/c-programs-name-no-file-a-file-server-holds.md`'s exit puts
        after libc is a file server's client. **Exit**: `tmpfs.rs`,
        `fs_rename.rs` and `file_cache.rs` are gone, `vfs.rs` resolves ROOT
        for exec alone, and `SYS_OPEN`, `SYS_READDIR`, `SYS_RENAME`,
        `SYS_MKDIR` and the rest of the path family are gone from `sdk/abi`.
      - **The block half**, with step 10: the block layer (`block.rs`), GPT
        (`gpt.rs`), USB storage (`drivers/usb_storage.rs`), the refusal chain
        (`BudgetExpired`, `until_answered`, `DEADMAN`, `between_attempts`) and
        `SYS_PARTITION_READ/WRITE`, all of which serve the stick until usbd
        does. **Exit**: `BudgetExpired`, `DEADMAN` and `between_attempts`
        appear nowhere in the tree.

      Each half measures the kernel's lines and its longest interrupts-off
      and preemption-off windows, by stage 6 step 2's rule.
   10. **usbd**, stage 5's second half: the whole xHCI moves, HID to the
       keyboard claim and mass storage over `toyos-blockring`. A USB
       keyboard's transitions reach the kernel only as usbd's keyboard claim.
       Its decoders are `toyos-xhci`'s Bulk-Only and SCSI modules and
       `toyos-usbhid`. **Exit**, on the T14: `/log` survives usbd killed
       mid-batch, and the keyboard keeps working while a stick misbehaves.
       **Partly done** (#827), its stage 1: the test-only
       `xhci-leave=<vendor>:<device>` leaves one controller to a claim, and
       `/system/bin/usbd` drives it — the handoff, a reset, its rings in one
       grant, MSI through the claim, and every device enumerated and named
       over `inspect`; no class is bound, and no shipped image runs it.
       **Left**, first the orchestrator's stage-0 ruling that pcidev arms MSI
       and keeps MSI-X Enable clear where the table shares the register BAR,
       then the whole xHCI, one usbd per controller (the orchestrator's other
       stage-0 ruling). pcidev today arms MSI only on a function with no MSI-X, and
       maps no BAR that holds the table (`kernel/src/pcidev/mod.rs`). QEMU's
       xHCI keeps its MSI-X table in the BAR that holds the registers, which
       refuses usbd's claim as it refuses diskserver's
       (`issues/a-controller-whose-msix-table-is-in-bar-0-cannot-be-driven-from-userland.md`),
       and the T14's has none on its capability walk, whose end is not read
       (`issues/every-driver-is-still-in-the-kernel.md`). The ruling's exit:
       usbd's claim on QEMU's `qemu-xhci` with its MSI-X left on is handed
       over armed on MSI, with MSI-X Enable clear.

       QEMU's spare today is `qemu-xhci,msix=off,msi=on`: with MSI-X off and
       MSI left `auto` it offers no MSI either, and the claim is refused for
       it. Read on the T14 (stage 0, `boot:testcases` readbacks): firmware
       owns neither controller. 00:0d.0, 8086:9a13, the Type-C one (port 1
       USB 2.0, ports 2..=5 USB 3.1) and 00:14.0, 8086:a0ed (ports 1..=12 USB
       2.0, 13..=16 USB 3.1) each read `USBLEGSUP 0x01002201` and
       `USBLEGCTLSTS 0xe0000000` — firmware's semaphore clear, every SMI
       enable clear, the three status bits latched — and each is armed on
       MSI. Neither 00:0d.0, whose release
       `usbd_drives_the_type_c_controller` reads as `reset by nothing
       (Express: no capability; AF: no capability; PM: No_Soft_Reset set)`,
       nor QEMU's `qemu-xhci` advertises a reset, so a claim on either
       inherits the last holder's ranges as residue; which reset 00:14.0
       advertises no boot has printed.

       Two compromises until this step, owned by it, whose exit is the
       kernel's copy deleted with the kernel's driver.
       `toyos-xhci`'s `descriptor` and the kernel's `parse_config`
       (`kernel/src/drivers/xhci/device.rs`) are two decoders of the same
       device-chosen bytes with different refusal rules — the kernel ends its
       walk on a zero length and clamps, the crate refuses a length below 2;
       its `xhci-descriptor-selftest` actuator goes with it.
       `toyos-xhci`'s `xecp` and the kernel's
       `kernel/src/drivers/xhci/legacy.rs` each carry the extended-capability
       walk, the firmware handoff's verdict and the SMI-off word; the
       orchestrator ruled the kernel's copy is not moved onto the crate's,
       since it goes whole here, and its `xhci-xecp-selftest` actuator goes
       with it.
5. **USB by userland**, with discovery and recovery written once as
   straight-line code: step 10, and this exit. **Exit**: no interrupts-off
   window longer than a register access, read by stage 6 step 2's
   instrument, and keyboard input keeps flowing while a stick misbehaves.
   **Partly done**: step 10's stage 1 (#827). **Left**: the rest of step 10,
   then this exit.
6. A handler posts its device's `Watch` and ends its interrupt; the thread
   waiting on that watch does the work, and no step creates a kernel thread.
   `irq_ring`, the driver list in `drain_irqs` and the idle loop's device
   checks are gone by step 5. Each step measures the kernel's lines against
   stage 6's first commit, and the longest interrupts-off and preemption-off
   windows against the readings step 2's exit puts in the tree, by that
   exit's rule. Steps 3 and 4 do not land alone: they land with stage 7 step 2
   and with usbd. **Partly done**: step 1, and step 2's instrument.
   1. **Interrupts post.** A post is legal in a handler: the watches a handler
      posts, and the completions of a ring they complete into, sit behind
      interrupts-off locks nothing allocates or frees under, and every other
      watch's lock leaves interrupts open. A claimed function's vector, the
      IOMMU's refusal and both audio backends post from the handler, and
      `irq_ring`'s `UserDev` and `Audio` and their arms in `drain_irqs` go.
      The thread is the holder's: netstack's, diskserver's, soundserver's mix
      thread, and an `isa` claim's holder. **Exit**:
      `handler_post_without_a_pass`, a vector taken on a CPU holding
      preemption off, inside a post of its own watch, inside a completion
      into a ring polling it, or inside that ring's own watch, posting once
      that section lets go and before any pass, red on the base; the watch's
      loom models over the new post. **Done** (#634). #660 cut
      `handler_post_without_a_pass`; the guest-suite track's stage C owes it
      as a T14 row, and the owner's testing ruling (#890) keeps it, since what
      it checks is the kernel's alone.
   2. **The windows, measured**: the longest interrupts-off and
      preemption-off windows per CPU, reported beside the IRQ census and fed
      by each architecture's masking primitives and entries, the number the
      ARM track's stage 4 owes as well. **Partly done** (#649): the
      `mask-windows` kernel and the T14's `mask_windows` row; the instrument
      reads back a window of known length on the T14, which is x86 metal,
      and the row's judge refuses a boot that reads it back shorter than it
      was held or at more than twice that. **Left**, the exit: neither window
      is longer with step 1 than with it reverted on the tree that carries
      the instrument, the patch posted on #649 being what reverted means.
      That comparison is the median of at least five interleaved boots an
      arm, each read from the load's own report with a report taken as the
      load starts, against a tolerance stated before measuring, the reverted
      arm's own spread, with the tail reported beside it, under a load in
      which a handler step 1 changed posts into a watch that reaches a ring
      with parked threads
      (`issues/a-process-lengthens-an-interrupts-off-walk-by-the-threads-it-parks-on-one-ring.md`'s
      second bullet). `ring_park_herd` is not that load: its walks start in
      `SYS_INBOX_SUBMIT`, which runs with interrupts masked on both arms, and
      it reaches no handler step 1 changed.
   3. **The i8042's thread is ps2server's** (stage 7 step 2): `irq_ring`'s
      `I8042`, `keyboard_controller::service` and the idle loop's
      `verdict_due` go with the kernel's driver. **Exit**: that step's.
      **Not started.**
   4. **xHCI's thread is usbd's** (stage 4 step 10): `Xhci`,
      `poll_if_pending` and `port_work_pending` go with the kernel's driver,
      and `irq_ring` and `sync::OwedLock`, whose one user is `XHCI`, with
      them. **Exit**: step 10's. **Not started.**
   5. **The pass is the scheduler's.** `drain_irqs` goes, and the TCO feed
      stays on the pass, since what it proves is that passes run. **Exit**:
      `drain_irqs` and the idle loop's device checks are gone, both windows
      are measured against step 2's readings by its rule, and the exits of
      `issues/an-irq-watchs-freeing-cancel-compiles-in-a-handler.md` and
      `issues/nothing-fails-when-a-devices-release-or-close-stops-answering-its-polls.md`
      are met. **Not started.**
7. **The i8042 leaves the kernel.** Owner ruling, 2026-09-28: drivers are
   userland, and a dead kernel takes no input, with no emergency way.
   **Partly done**: step 1.
   1. **The panic console takes no input, and an `isa` claim grants a process
      exact ports through the TSS I/O permission bitmap and its ISA lines as
      records** (`kernel/src/isa.rs`). **Done** (#592).
   2. **ps2server**, the server over that claim, feeding the kernel's keyboard
      and mouse streams so the merge with USB HID stays where it is; the
      kernel's driver, its vector, its actuators and the
      `keyboard_controller` seam deleted. A keyboard claim is refused while no
      source exists, which the supervisor's order of endowment then decides.
      **Exit**: no i8042 code in `kernel/src` (`arch/x86_64/i8042/`,
      `arch/x86_64/idt/i8042.rs` and both architectures'
      `keyboard_controller` gone), and typing resumes after ps2server is
      killed and restarted. **Not started.**

## Standing

No device wait, spin or poll happens while holding a spinlock or inside a
scheduler pass; interrupts post, threads wait. Every stage measures the kernel
it leaves behind (lines, the longest interrupts-off window, the longest
preemption-off window) against the one it started from.
