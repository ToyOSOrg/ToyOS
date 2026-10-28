---
status: open
kind: track
opened: 2026-09-25
---

# The kernel is small: interrupts post, threads wait, and storage lives in userland

Owner ruling, 2026-09-25: the kernel is to be super small, super performant
and safe. This track holds that, and supersedes the ordering of
`issues/every-driver-is-still-in-the-kernel.md`, which stays as the evidence
each stage closes. Owner: the orchestrator, which
dispatches each step and each test this track owes. An exit holds the
kernel's part of a restart, and named module tests hold each program's
(#890).

## What is left, in order

1. **Step 9's file half** (stage 4). libc is a file server's client
   through `toyos::files`, as std is.
2. **Step 8's exit** (stage 4). Its empty view needs step 9's file half:
   until then the kernel serves `/tmp` to every process, and every view holds
   `/system` read-only.
3. **P1 of the Wi-Fi track** (`issues/there-is-no-wifi.md`): a BAR stays
   where firmware put it, handed over in 4 KiB pages with its MSI-X table's
   page, under the owner's Q2 ruling. The orchestrator ruled on #900 that
   this route serves usbd's claim (step 10).
4. **Step 10's metal rows still owed** (step 10, below): a reset, a tone and
   `mask_windows` under a write to a stick through usbd, which no bench row
   makes since the benches boot off their NVMe
   (`issues/no-bench-row-drives-usb-storage-since-the-benches-boot-off-nvme.md`).
5. **Stage 6 step 5, then stage 5's windows.**

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
   check covers both), and hands the kernel its address and length. No storage
   driver is needed to boot: the supervisor's file server serves ROOT out of
   that memory (owner ruling 2026-10-21, step 9).
   **Exit**: the machine boots in QEMU and on the T14 with the kernel's NVMe
   and USB storage paths never touched before the supervisor runs. The
   boot-time cost on the stick and on NVMe is measured.
   **Partly done** (#506; T14 run 143 green), the stick's cost measured, and
   with step 10 the kernel drives no storage at all: no storage command is
   issued before the supervisor runs, since the supervisor starts every disk
   driver. **Left**: the NVMe cost, which waits on the self-update track
   (above).
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
   the transport, for diskserver's part by `wire::Opened::over`, whose type
   makes a server's ends before its answer, and by the client test
   `issues/nothing-reopens-a-diskserver-session-over-a-used-page.md` owes, a
   session reopened over the page a killed server left, by fileserver's
   `a_directory_rename_is_whole_under_one_name_wherever_the_server_is_killed`
   for DATA, and for LOG by the FAT test step 10 owes. **Partly done**
   (#525, #536, #971): NVMe and the stick are driven from userland, and the
   kernel's NVMe driver, USB storage, block layer and refusal chain are gone;
   the `nvme` driver and usbd offer their disks to diskserver, the block
   service. **Left**: the page cache, with step 9's file half, and the client
   test `issues/nothing-reopens-a-diskserver-session-over-a-used-page.md`
   owes.
4. **Where the data filesystem lives** — owner ruling, 2026-09-26, from the
   roasted proposal: userland file servers, the VFS a client library, and a
   program's view a set of directory capabilities the supervisor hands it,
   each resolved by its server. The page cache lives in the file server. One
   file server per role (LOG, DATA, BOOT), so one crashing cannot take another
   down. A file-server crash is survivable, not invisible: data survives on
   disk, clients reopen, and the few states that cannot be recovered answer
   `Gone`; invisible restart can come later. The kernel keeps no filesystem
   of ROOT (owner ruling 2026-10-21, step 9, which replaced this ruling's
   in-memory read path and exec from it). USB storage stays in the kernel until
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
   **Partly done**: steps 1, 2, 3, 4, 6 and 7 are done, and 5, 8, 9 and 10
   partly.

   Stages 3 and 4 are built as these steps:
   1. **`toyos-blockring`**, the protocol, pure. **Done** (#525).
   2. **`SYS_DEVICE_DMA_MAP`**. **Done** (#525).
   3. **diskserver**, NVMe in userland. **Done** (#525; the restart, #536).
   4. **Partitions in diskserver**. **Exit**: a partition held by one session
      at a time, the idle slot claimed by GUID and written through
      diskserver, and `SYS_PARTITION_READ/WRITE` deleted once no disk the
      kernel drives serves a claim. **Done** (#525, #826, #971): sessions, a
      block connector that opens only the partitions its badge grants, the
      idle slot written through one, and the stick's partitions diskserver's
      through usbd; the kernel drives no disk, and `SYS_PARTITION_READ/WRITE`
      are deleted.
   5. **Spawn and dlopen from an image handle.** **Exit**: a program in
      `/apps` runs having been read by userland, its faults served from the
      image object and not the VFS; and `SYS_DLOPEN` takes no path.
      **Done**: spawn (#536, `SpawnArgs::image`), and dlopen: libc's and the
      SDK's `dlopen` read the library through `toyos::files::read_image`
      wherever it is served, `SYS_DLOPEN` loads it from the image handle
      (`DlopenArgs`) under a name it opens nothing by, and the kernel's
      cross-process library cache is deleted, each process holding its own
      library pages (orchestrator, under the memory design, whose later
      stages bring sharing back through memory objects).
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
      **Done** (#536, and on the stick #971).
   8. **Views as capabilities**, the isolation track's stage 1
      (`issues/every-program-sees-only-the-files-it-was-given.md`).
      **Exit**: that stage's, run against fileserver: an escape suite red on
      a resolver that walks the global tree and green on fileserver's, and a
      process given an empty view naming nothing. **Partly done** (#729,
      #742): a connection is the grant the supervisor minted on its role's
      one port, and `fs_escape` is refused a symlink that climbs out, a `..`
      on the wire and a directory it was not given. **Left**: the rest of the
      escape suite, a rename racing a lookup among it, shown red on a
      global-tree resolver; and the empty view, which names `/tmp` while the
      kernel serves it (step 9's file half).
   9. **Delete the kernel storage stack.** **Partly done** (#536). Two
      halves are left, and they land apart:
      - **The file half**: the VFS, tmpfs (`tmpfs.rs`, `fs_rename.rs`), the
        page cache (`file_cache.rs`) and the file syscalls. `/tmp` leaves the
        kernel per session, as the owner ruled ("Per session", 2026-10-03, in
        the isolation track). **`/system` is read over the file protocol**,
        which std and libc both reach through `toyos::files` (owner ruling
        2026-10-21, "Removing the boot filesystem exception"):
        the loader hands the kernel the supervisor's and the file server's
        executables beside ROOT, each a section of the slot's signed header;
        the kernel keeps their pages read-only, starts the supervisor from its
        image and hands it the other two, and the supervisor starts the file
        server's SYSTEM role from its image, serving ROOT out of that memory,
        before it reads the manifest. The kernel's ROOT mount, its bcachefs
        adapter and its spawn by `/system` path are deleted. **Exit**:
        `tmpfs.rs`, `fs_rename.rs`, `file_cache.rs` and `vfs.rs` are gone, and
        `sdk/abi` has no `SYS_OPEN` (9),
        `SYS_STAT` (16), `SYS_READDIR` (17), `SYS_DELETE` (18), `SYS_CHDIR`
        (20), `SYS_GETCWD` (21), `SYS_RENAME` (52), `SYS_MKDIR` (53),
        `SYS_RMDIR` (54), `SYS_SYMLINK` (81) or `SYS_READLINK` (82).
      - **The block half**, with step 10: the block layer (`block.rs`), GPT
        (`gpt.rs`), USB storage (`drivers/usb_storage.rs`), the refusal chain
        and `SYS_PARTITION_READ/WRITE`. **Exit**: `git grep -n -e
        BudgetExpired -e DEADMAN -e between_attempts -e until_answered --
        kernel` finds nothing. **Done** with step 10.

      Each half measures the kernel's lines and its longest interrupts-off
      and preemption-off windows, by stage 6 step 2's rule.
   10. **usbd**, stage 5's second half, and stage 4 of
       `issues/a-device-function-is-driven-from-its-arrival-to-its-end.md`:
       usbd is a `drives` row, one process per xHCI controller. The whole
       xHCI is usbd's: HID delivered to the owner of input over the `input`
       port, each bound collection its own `source` with its own `gone`, and
       each stick a disk offered to the block service on `disk`, whose
       sessions usbd serves over `toyos-blockring`
       (`issues/a-device-function-is-driven-from-its-arrival-to-its-end.md`'s
       block ruling, which replaces #901's `block@` ports). **Done**: the
       kernel's xHCI, USB storage, block layer, GPT, keyboard and mouse
       streams, partition claims and `sync::OwedLock` are deleted, with the
       xHCI and USB actuators. Every guest boot's stick goes through usbd,
       and so does the HID of an image whose usbd receives `input`
       (`images/console.toml`'s, `a_usb_key_reaches_the_console`); a test
       image's usbd receives no `input`, so its HID is bound and let go at
       the first `NotGiven`. The machine's stop has usbd reset every connected
       port and its controller (`Hc::stop`) before the kernel's, as the
       kernel's xHCI stop did. **The exit's evidence**: the kernel's
       part by `usbd_drives_the_spare` on the default `qemu-xhci` and
       `usbd_drives_the_type_c_controller`; the kernel's input streams gone, with no
       `Keyboard` or `Mouse` in `sdk/abi`'s `DeviceType` or `FileType`; the
       owner of input's by `toyos-input`'s
       `a_connections_end_releases_every_device_it_carried_and_a_gone_only_its_own_source`;
       usbd's by `toyos-xhci`'s stub tests, under the names the exit gave
       them — `a_controller_that_never_answers_still_releases_the_port`,
       `a_recovery_the_controller_refuses_lets_the_device_go`,
       `no_sequence_breaks_an_invariant` — and the owed
       `a_stick_whose_bulk_endpoint_never_answers_leaves_a_keyboard_on_the_same_controller_delivering_every_report`;
       `/log`'s by the `toyos-blockring` and `fat.rs` tests it named, and by
       `the_log_lands_on_the_boot_stick` on QEMU's stick; the stop's by
       `the_stop_resets_every_connected_port_then_halts_resets_and_unpowers_the_controller`
       and the same row's judge of usbd's exit; the HID path's on a real xHC
       by `a_usb_key_reaches_the_console`.
       A machine whose units cannot remap refuses every claim, usbd's
       included, so it drives no USB and its stick receives no `/log`: the
       metal rows that asked for that machine (`iommu-no-remap`) are deleted,
       and the refusal is QEMU's to hold, by
       `usbd_refused_without_amdvi_remapping` and `amdvi_firmware_left`.
       **Left**: #901 section 9's three metal judgements under a userland
       load on a stick — a deadline reset, a tone
       (`issues/a-megabyte-written-to-the-stick-starves-a-tone-beside-it.md`'s
       exit) and `mask_windows` — which no bench row makes since the benches
       boot off their NVMe; `the_disk_survives_a_reset_under_load`,
       `tone_beside_a_log_write` and `mask_windows` under `log_write` judge the
       same under a write to the NVMe. #971's first metal boots
       came back with no `logkeeper` file on the stick, which every metal
       row reads; QEMU's stick boot (`the_log_lands_on_the_boot_stick`) has
       it. And the client test
       `issues/nothing-reopens-a-diskserver-session-over-a-used-page.md`
       owes, which `/log`'s evidence names.

5. **USB by userland**, with discovery and recovery written once as
   straight-line code: step 10, and this exit. **Exit**: step 10's, and no
   interrupts-off window longer than a register access, read by stage 6 step
   2's instrument. **Partly done**: step 10. **Left**: step 10's metal rows,
   then the windows.
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
      **Done** (#899).
   4. **xHCI's thread is usbd's** (stage 4 step 10): `Xhci`,
      `poll_if_pending` and `port_work_pending` go with the kernel's driver,
      and `irq_ring` and `sync::OwedLock`, whose one user is `XHCI`, with
      them. **Exit**: step 10's. **Done** with step 10.
   5. **The pass is the scheduler's.** `drain_irqs` goes, and the TCO feed
      stays on the pass, since what it proves is that passes run. **Exit**:
      `drain_irqs` and the idle loop's device checks are gone, both windows
      are measured against step 2's readings by its rule, and the exits of
      `issues/an-irq-watchs-freeing-cancel-compiles-in-a-handler.md` and
      `issues/nothing-fails-when-a-devices-release-or-close-stops-answering-its-polls.md`
      are met. **Partly done**: `drain_irqs` went with step 10, whose xHCI
      was the last driver it polled. **Left**: the windows and the two
      issues' exits.
7. **The i8042 leaves the kernel.** Owner ruling, 2026-09-28: drivers are
   userland, and a dead kernel takes no input, with no emergency way.
   **Done**.
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
      **Done** (#899).

## Standing

No device wait, spin or poll happens while holding a spinlock or inside a
scheduler pass; interrupts post, threads wait. Every stage measures the kernel
it leaves behind (lines, the longest interrupts-off window, the longest
preemption-off window) against the one it started from.
