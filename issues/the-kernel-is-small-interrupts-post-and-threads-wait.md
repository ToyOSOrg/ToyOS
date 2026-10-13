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
dispatches each step and each test this track owes. An exit holds the
kernel's part of a restart, and named module tests hold each program's
(#890).

## What is left, in order

1. **ps2server**: stage 7 step 2, with stage 6 step 3.
2. **Step 9's file half** (stage 4), once libc is a file server's client
   (`issues/c-programs-name-no-file-a-file-server-holds.md`).
3. **Step 8's exit** (stage 4). Its empty view needs step 9's file half:
   until then the kernel serves `/system` and `/tmp` to every process.
4. **P1 of the Wi-Fi track** (`issues/there-is-no-wifi.md`): a BAR stays
   where firmware put it, handed over in 4 KiB pages with its MSI-X table's
   page, under the owner's Q2 ruling. The orchestrator ruled on #900 that
   this route serves usbd's claim (step 10).
5. **usbd's whole xHCI**: step 10, with stage 6 step 4, step 4's tail and
   step 9's block half, which serve the stick until usbd does.
6. **Stage 6 step 5, then stage 5's windows.**

Beside that order, each waiting on something outside it:
- stage 1's NVMe boot-time measurement, on a ROOT on the T14's NVMe, which
  the self-update track installs
  (`issues/the-machine-updates-itself-without-ubuntu.md`);
- stage 6 step 2's comparison, on its load;
- step 5's dlopen, on the same libc client as step 9's file half.

## Stages

1. **The system image is loaded into memory by the loader.** The UEFI loader
   reads the selected slot's ROOT through the firmware's own block I/O, checks
   its signature on the bytes in memory (the self-update's signing, so one
   check covers both), and hands the kernel its address and length. The kernel
   mounts ROOT from memory and needs no storage driver to boot.
   **Exit**: the machine boots in QEMU and on the T14 with the kernel's NVMe
   and USB storage paths never touched before the supervisor runs. The
   boot-time cost on the stick and on NVMe is measured.
   **Partly done** (#506; T14 run 143 green), the stick's cost measured.
   **Left**: `kernel_main` runs `xhci::init` and
   `gpt::probe_usb_disks` after that spawn and before `smp::set_ready`, so
   before the supervisor's first instruction
   (`issues/storage-still-comes-up-before-the-supervisor-runs.md`); both go
   with the kernel's xHCI at step 10. The NVMe cost waits on the self-update
   track (above).
2. **One way to wait.** **Done** (#513).
3. **Storage in userland.** NVMe and USB mass storage are programs that claim
   their device through its IOMMU domain, as netstack does, and serve a block
   protocol over shared-memory rings. Partition claims move into that service.
   The kernel's NVMe driver, USB storage, block layer, page cache and the
   whole budget and refusal chain are deleted. **Exit**: NVMe keeps more
   than one command in flight, measured; a dead holder's claim is handed
   over again, held by `usbd_drives_the_spare` (guest) and
   `usbd_drives_the_type_c_controller` (T14); and what a disk service's
   death leaves is held by `toyos-blockring`'s
   `every_request_is_answered_exactly_once`,
   `what_a_flush_calls_durable_is_on_the_medium` and
   `what_was_on_the_wire_at_the_end_is_refused_and_what_was_not_waits` for
   the transport, by diskserver's
   `a_session_opened_over_a_killed_servers_page_is_told_after_its_ends_are_made`
   for diskserver's part, a session reopened over the page a killed server
   left, red under the mutation that moves the server's stores after its
   answer, by fileserver's
   `a_directory_rename_is_whole_under_one_name_wherever_the_server_is_killed`
   for DATA, and for LOG by the FAT test step 10 owes. **Partly done**
   (#525, #536): diskserver drives NVMe, and the kernel's NVMe driver is
   gone. **Left**: on the T14, `/log` and `/home` are on the
   stick, which the kernel drives until step 10; the kernel's USB storage,
   block layer, page cache and refusal chain go at step 9.
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
   **Owed**, the orchestrator's to dispatch, for the restart beyond the
   bytes stage 3 holds: a module test in `sdk/toyos/src/fs.rs` that a call
   on an id from an ended connection answers `Gone` and a path call
   reconnects once, red with the generation check removed; and a module
   test in `system/supervisor` that an ended role is started again until
   `toyos_manifest::RESTARTS` ends inside the window and is then closed with
   its clients answered `Gone`, red on a supervisor that restarts without
   bound, and that a restart whose partition claim another process holds is
   refused by name, red on a supervisor that starts the role again while its
   claim is held or that drops the name from the `AlreadyExists` refusal
   (`refused` in `system/supervisor/src/main.rs`).
   **Partly done**: steps 1, 2, 3, 6 and 7 are done, and 4, 5, 8, 9 and 10
   partly.

   Stages 3 and 4 are built as these steps:
   1. **`toyos-blockring`**, the protocol, pure. **Done** (#525).
   2. **`SYS_DEVICE_DMA_MAP`**. **Done** (#525).
   3. **diskserver**, NVMe in userland. **Done** (#525; the restart, #536).
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
      (`issues/a-package-cannot-ship-its-own-libraries.md`). Its exit: a
      library in `/apps` loads having been read by userland, its faults
      served from the image object, and `SYS_DLOPEN` takes no path.
   6. **fileserver for DATA over diskserver.** **Done** (#536). #660 cut
      its guest tests and #890 keeps them cut.
      `apps_and_home_are_one_filesystem`'s claim, `/apps` and `/home` one
      volume, is held by fileserver's
      `a_directory_rename_is_whole_under_one_name_wherever_the_server_is_killed`,
      which renames `home/staged` to `apps/pkg` in one volume.
      `home_overwrite_reads_back`'s, a recorded failure (`e2bd52e83`: a
      same-length `/home` overwrite read back 0 bytes), nothing holds.
      **Owed**, by the orchestrator, waiting on nothing: a test in
      `system/fileserver/src/data.rs` that holds a synced file open,
      reopens it with truncate, writes as many other bytes, closes the held
      handle then the writer, syncs, remounts and reads the new bytes back
      whole, shown red on an `open` whose truncate makes a new file under a
      held file's name.
   7. **fileserver for LOG and BOOT; logkeeper on the client library.**
      **Done** (#536); on a disk the kernel drives, fileserver opens LOG and
      BOOT through the kernel's partition claims, which step 10 deletes.
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
   9. **Delete the kernel storage stack.** **Partly done** (#536). Two
      halves are left, and they land apart:
      - **The file half**: the VFS down to ROOT's resolver, tmpfs (`tmpfs.rs`,
        `fs_rename.rs`), the page cache (`file_cache.rs`) and the file
        syscalls. `/tmp` leaves the kernel per session, as the owner ruled
        ("Per session", 2026-10-03, in the isolation track), and `/system` is
        read over the file protocol, which
        `issues/c-programs-name-no-file-a-file-server-holds.md`'s exit puts
        after libc is a file server's client. **Exit**: `tmpfs.rs`,
        `fs_rename.rs` and `file_cache.rs` are gone, `vfs.rs` resolves ROOT
        for exec and `SYS_DLOPEN` alone, and `sdk/abi` has no `SYS_OPEN` (9),
        `SYS_STAT` (16), `SYS_READDIR` (17), `SYS_DELETE` (18), `SYS_CHDIR`
        (20), `SYS_GETCWD` (21), `SYS_RENAME` (52), `SYS_MKDIR` (53),
        `SYS_RMDIR` (54), `SYS_SYMLINK` (81) or `SYS_READLINK` (82).
        `SYS_DLOPEN` (55) is step 5's.
      - **The block half**, with step 10: the block layer (`block.rs`), GPT
        (`gpt.rs`), USB storage (`drivers/usb_storage.rs`), the refusal chain
        and `SYS_PARTITION_READ/WRITE`, all of which serve the stick until
        usbd does. **Exit**: `git grep -n -e BudgetExpired -e DEADMAN -e
        between_attempts -e until_answered -- kernel` finds nothing.

      Each half measures the kernel's lines and its longest interrupts-off
      and preemption-off windows, by stage 6 step 2's rule.
   10. **usbd**, stage 5's second half: the whole xHCI moves, HID delivered
       to the owner of input over the `input` port and mass storage over
       `toyos-blockring`. A USB keyboard's transitions never reach the
       kernel, whose keyboard and mouse streams go when their last kernel
       source leaves (the ruling on #899, applied to usbd on #901). It
       depends on stage 7 step 2 landing first, because at step 10 usbd's
       is the kernel's last input source (the ruling on #904,
       https://github.com/ToyOSOrg/ToyOS/pull/904#issuecomment-6139079945).
       Its decoders are `toyos-xhci`'s Bulk-Only and SCSI modules and
       `toyos-usbhid`. **Exit**: the kernel's part, usbd's claims handed
       over again at its death: the controller's by `usbd_drives_the_spare`
       on the default `qemu-xhci` (below) and
       `usbd_drives_the_type_c_controller`; there is no keyboard claim. The
       kernel's input streams, deleted: `keyboard.rs`, `mouse.rs` and
       `input_merge_test.rs` gone from `kernel/src`, and no `Keyboard` or
       `Mouse` in `sdk/abi`'s `DeviceType` or `FileType`. The
       owner of input's, a key usbd held at its death released:
       `toyos-input`'s test that a connection's end releases every device it
       carried and a `gone` only its own `source`, red with `Merge`'s release
       on a connection's end removed, and red with `gone` releasing its whole
       connection. usbd's, a device that stops answering
       costing only itself: `toyos-xhci-sim`'s
       `a_controller_that_never_answers_still_releases_the_port`,
       `a_recovery_the_controller_refuses_lets_the_device_go` and
       `no_sequence_breaks_an_invariant`, with one sim test this step owes:
       a stick whose bulk endpoint never answers leaves a keyboard on the
       same controller delivering every report, red on a usbd that waits on
       the stick before it reads the keyboard's ring. `/log`'s, what a killed
       usbd had on the wire: stage 3's `toyos-blockring` tests for the
       transport, and for usbd's session open diskserver's
       `a_session_opened_over_a_killed_servers_page_is_told_after_its_ends_are_made`,
       moved with the open, red under the mutation that moves usbd's stores
       after its answer; with one test this step owes in
       `system/fileserver/src/fat.rs`: a disk stopped after each block write
       in turn of an append and its sync mounts at every stop and reads back
       all the last whole sync held, red on a sync that writes a growing
       file's directory entry before its chain.

       **Partly done** (#827), its stage 1: the test-only
       `xhci-leave=<vendor>:<device>` leaves one controller to a claim, and
       `/system/bin/usbd` drives it — the handoff, a reset, its rings in one
       grant, MSI through the claim, and every device enumerated and named
       over `inspect`; no class is bound, and no shipped image runs it.
       **Left**, first P1 (order item 4), then the whole xHCI, one usbd per
       controller (the orchestrator's stage-0 ruling). pcidev today arms MSI
       only on a function with no MSI-X, and maps no BAR that holds the
       table (`kernel/src/pcidev/mod.rs`). QEMU's xHCI keeps its MSI-X table
       in the BAR that holds the registers, which refuses usbd's claim as it
       refuses diskserver's
       (`issues/a-controller-whose-msix-table-is-in-bar-0-cannot-be-driven-from-userland.md`,
       whose exit P1 rewrites), so QEMU's spare today is
       `qemu-xhci,msix=off,msi=on` (`XHCI_SPARE` in `tests/common/qemu.rs`).
       P1 serves usbd when that spare is `qemu-xhci` with its MSI-X left on
       and `usbd_drives_the_spare` passes on it. The T14's xHCI has no MSI-X
       on its capability walk, whose end is not read
       (`issues/every-driver-is-still-in-the-kernel.md`).

       Read on the T14 (stage 0, `boot:testcases` readbacks): firmware owns
       neither controller. 00:0d.0, 8086:9a13, the Type-C one (port 1 USB
       2.0, ports 2..=5 USB 3.1) and 00:14.0, 8086:a0ed (ports 1..=12 USB
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
   straight-line code: step 10, and this exit. **Exit**: step 10's, and no
   interrupts-off window longer than a register access, read by stage 6 step
   2's instrument. **Partly done**: step 10's stage 1 (#827). **Left**: the
   rest of step 10, then this exit.
6. A handler posts its device's `Watch` and ends its interrupt; the thread
   waiting on that watch does the work, and no step creates a kernel thread.
   `irq_ring`, the driver list in `drain_irqs` and the idle loop's device
   checks are gone by step 5. Each step measures the kernel's lines against
   stage 6's first commit, and the longest interrupts-off and preemption-off
   windows against the readings step 2's exit puts in the tree, by that
   exit's rule. Steps 3 and 4 do not land alone: they land with stage 7 step 2
   and with usbd. **Partly done**: step 1, and step 2's instrument.
   1. **Interrupts post.** **Done** (#634). #660 cut its guest test,
      `handler_post_without_a_pass`; the guest-suite track's stage C owes it
      as a T14 row, which #890 leaves standing, since what it checks is the
      kernel's alone.
   2. **The windows, measured**: the longest interrupts-off and
      preemption-off windows per CPU, reported beside the IRQ census and fed
      by each architecture's masking primitives and entries, the number the
      ARM track's stage 4 owes as well. **Partly done** (#649): the
      `mask-windows` kernel and the T14's `mask_windows` row. **Left**, the
      exit: neither window
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
      exact ports and its ISA lines.** **Done** (#592).
   2. **ps2server**, the server over that claim, delivering to the owner
      of input over the `input` port, where userland merges it with USB HID
      (the ruling on #899); the kernel's merge (`HELD`, `merged_buttons`,
      `SOURCE_EXISTS`) is deleted, and its streams carry its own sources'
      records unmerged until usbd takes the last of them; the
      kernel's driver, its vector, its actuators and the
      `keyboard_controller` seam deleted. ps2server's one kernel claim is the
      `isa` row; it takes no keyboard claim, and the owner reads its
      transitions from its `input` acceptor's connections.
      **Exit**: no i8042 code in the kernel (`arch/x86_64/i8042/`,
      `arch/x86_64/idt/i8042.rs` and both architectures'
      `keyboard_controller` gone from `kernel/src`, and `toyos-ps2`, whose
      one kernel user is `kernel/src/arch/x86_64/i8042/mod.rs`, gone from
      `kernel/Cargo.toml`); a kernel test this step owes, that the `isa`
      row whose holder dies is handed to the next claimant with none of the
      dead holder's records pending (`isa_lines_reach_their_holder`), red
      on a kernel whose release keeps the dead holder's record;
      and ps2server's restart mid-packet held by `toyos-ps2`'s
      `a_lost_scancode_byte_never_wedges_the_prefix_state` and
      `a_stream_truncated_at_any_offset_resyncs_within_two_packets`.
      **Not started.**

## Standing

No device wait, spin or poll happens while holding a spinlock or inside a
scheduler pass; interrupts post, threads wait. Every stage measures the kernel
it leaves behind (lines, the longest interrupts-off window, the longest
preemption-off window) against the one it started from.
