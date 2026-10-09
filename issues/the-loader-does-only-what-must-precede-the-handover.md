---
status: open
kind: track
opened: 2026-09-28
---

# The loader does only what must happen before the handover

The owner's bounds:

- the loader does only what must precede the handover;
- the hardware clock keeps UTC;
- the kernel calls no UEFI service, and every UEFI call ToyOS makes is the
  loader's, before `ExitBootServices` (root `CLAUDE.md`);
- the anti-rollback floor counts a signed security version, raised only by a
  release that fixes a security hole;
- the loader calls UEFI through its own bindings (`bootloader/src/efi/`),
  never the `uefi` crate's (the owner's ruling on the dependency audit).

PR #539 does not land. Its pieces:

- stage 2 takes `toyos_update::entry::partition` and its two host tests;
- stage 5 takes the rest of `toyos_update::entry` and its host tests;
  `slots::Request` with its once slot and its boot-order word;
  `bootloader/src/request.rs`, which takes a request off the table before
  acting on it; `bootloader/src/bootvars.rs`' own entry and `BootOrder`
  write, with `arch::REMOVABLE_PATH`, the file that entry names;
  `update --boot-first`, `update --once` and
  `update_boot_first_puts_the_loader_first`, which also takes
  `update_boot_next_boots_the_entry_once`'s read-only half; the harness's
  `stick_readonly`; and `update_trial_writes_nothing_of_the_kept_slot`,
  rewritten for priorities;
- stage 6 takes the bench: `src/metalbench.rs`, `tests/common/bench.rs`,
  `tests/bench*case`, `--bench-image` with `build::bench_image`,
  `src/image.rs`' `update_of`, `tests/common/metal.rs`' `Reach`, `stage` and
  bench `invocation`, `--metal-via-ubuntu`, toybox `date`, `Ssh::probe` and
  `Ssh::fetch` with the ssh client's `probe` and `fetch` (the client is
  deleted, `issues/the-host-cannot-reach-the-t14-while-it-runs-toyos.md`),
  `build::AUTHORIZED_ON_ROOT`, and `src/bootlog.rs`' `LOADER_PREVIOUS_LOG`,
  `MOUNTED_FROM_MEMORY` and `BOOT_PARAMETER`;
- deleted: `update --boot-next <esp>` with `slots::Next::Esp`, `Guid::parse`,
  `bootvars.rs`' `BootNext` and entry-after-its-own writes, and
  `update_boot_next_boots_the_entry_once`'s other half; the panic handler's
  fall to the next boot entry, with the two issues #539 filed about it,
  `update_no_slot_boots_the_recovery_stick`, and the harness's
  `recovery_stick` and `RECOVERY_STICK_*`; the `bootvars::state` line;
  `policy::order`'s once and `policy::told`, whose rules `pass::decide` takes;
  `record.rs`' `once`, with the attempts file; `SLOT_ONCE`, because stage 5's
  `KernelArgs` carries a once boot as a field; `LOADER_IS`, because the
  loader names no hash of its own file.

Each stage lands on its own, in this order.

1. **What PR #583 lands.**
   - The RTC keeps UTC. The loader's `GetTime` zone read, `KernelArgs`' two
     zone fields, the kernel's `UTC_OFFSET_SECS` and its local/UTC split, the
     `rtc-zone-east` actuator, `wall_clock_zone` and logd's zone recovery go.
     FAT stamps, `SYS_CLOCK_REALTIME` and `SYS_CLOCK_EPOCH` read
     `clock::utc_secs`.
   - `rootbridge.rs`' hex dump goes, with `toyos-acpi`'s `Walk::bytes`.
   - Ten loader and `toyos-update` comments go, each named in #583's commits.
   - `KernelArgs::layout`, at offset 164, carries `LAYOUT`: `0x5459_0000 |
     size_of::<KernelArgs>() as u32`, so a size change moves the word with no
     literal to remember. At this stage that is `0x5459_0000 | 1272`, the size
     PR #583 asserts. The kernel compares it right after arming the panel and
     before `blackbox::arm`, and refuses any other value by name. `update`
     never replaces the ESP loader, so a slot's kernel can meet a loader built
     to another layout. Every field before the word keeps its offset in every
     layout.
   - The kernel logs `boot: IA32_TSC_ADJUST` beside its power-on report, and
     the loader no longer reads it. aarch64's `counter_origin` is deleted.
   - The loader's TCO arm stays until stage 8.
   - This stage is an ABI change.

   **Exit**: the loader applies no zone, and `bootloader/src` holds no
   `counter_origin`. `wall_clock_utc` stages a firmware zone of −120 minutes
   in PcRtc's `RTC` variable. It judges the log file's name, its FAT stamp,
   the probe's `realtime=` and `SYS_CLOCK_EPOCH` against the `-rtc base=`
   instant. It fails under a kernel that reads the RTC as local time two
   hours east, and under `SYS_CLOCK_REALTIME` patched to `utc_secs + 7200`.
   `kernel_args_layout_refused` boots with the `loader-writes-no-layout`
   actuator, which makes the loader write 0. It finds the kernel's refusal
   naming both words, and no `black box:` record before it. No test fails
   where the `IA32_TSC_ADJUST` line is missing.

2. **One boot disk.**
   - `toyos_update::entry::partition` is the one HARDDRIVE rule, taken by
     `boot_partition`, `rootimage::boot_disk` and `bootnext.rs`.
   - Every volume the loader opens (the slot's FAT partition, the log
     partition and the attempts file on it) is found on the boot disk, where
     exactly one match is taken. `loaderlog::volume_handle`'s machine-wide
     first match goes. Every stick written from one image carries the same
     unique GUIDs: `src/image.rs`'s `create_boot_image` draws them once per
     image.
   - A pass asks firmware once: one `LoadedImage` open, one device-path walk,
     one `Disk::open` and slot-table read, and one file reader.
     `load_file_bytes`, `MAX_ESP_FILE` and the unsound `alloc_uninit` go.

   **Exit**: `a_path_names_the_partition_of_its_one_hard_drive_node` and
   `a_partitions_disk_is_the_path_before_its_hard_drive_node` pass on the host,
   and fail under two mutations: cutting the disk before the path's last node,
   and taking a second HARDDRIVE node. `root_named_twice` plants a twin whose
   bytes differ, so a loader that reads the twin fails it. A second disk
   carrying the boot disk's log partition GUID leaves the log on the boot disk.
   The oracle is UEFI 2.10 §10.3.5.1.

3. **One floor per key, on a security version.**
   - The signed header's `version` is the security version: one constant in
     the tree, `toyos_update::SECURITY_VERSION`. It is raised by a reviewed PR
     that edits that constant alone. This stage adds this line to
     `.claude/agents/reviewer.md`'s "What to look for":

     ```markdown
     - **Security version.** A branch that raises `toyos_update::SECURITY_VERSION` changes nothing else and names in its body the security fix it ships, or it is a BLOCKER.
     ```

     `image::version_now` goes, and `Plan::version` defaults to the constant.
     The header's `FORMAT` rises, so a header carrying a build time is
     refused by name.
   - `floor::Scope` goes. Every loader keeps one floor per signing key, named
     for the key alone, under a prefix no build-time floor carries. `stale`
     and its deletions go.
   - The loader deletes every variable under the build-time prefix
     `ToyOSImageFloor-`. Only a loader older than this stage reads one, and a
     loader firmware boots in place of this one is already outside the floor.
   - No loader deletes another key's floor, because a deletion lowers it. The
     store keeps one 8-byte variable per key that has booted the machine.
   - `policy::installable` admits an image at or above the running image's
     and the idle slot's security version.
   - This stage deletes the throwaway-key bullet and the
     "`/system/bin/update`'s own check — newer than the running image" line of
     `issues/the-anti-rollback-floor-is-a-firmware-variable.md`, and
     the per-image floor in stage 1 of
     `issues/the-machine-updates-itself-without-ubuntu.md`. The floor
     issue's TPM 2.0 NV counter stays its exit: `TPM2_NV_Increment` moves one
     step, and so does a security version. The cost the owner accepted, an
     older image admitted at the floor's security version, is on the floor
     issue's list.

   **Exit**: the guest tests run the scope the owner's machine runs.
   `update_floor_is_the_images_own` checks three things. This key's floor,
   planted above the image's security version, refuses the image under a
   fresh log GUID. An older build at the floor's security version boots.
   Another key's floor holds the image to nothing and stays, and a planted
   `ToyOSImageFloor-` variable is gone. Putting the log GUID back into the
   name makes it fail. The oracle is the vars store as the host reads it
   (`fwvars::live` in `tests/common/fwvars.rs`): each floor's name and its
   UEFI 2.10 §8.2 attributes.

4. **The loader sound, with a typed handover.**
   - The loader's `#[panic_handler]` writes `loader: panicked at
     <file>:<line>: <message>` through `loaderlog`, then powers the machine
     off. It never resets: a panic with a fixed cause would reset into itself.
     The refused-floor site stops writing its reason to `loader.log` before
     its `panic!`: the handler writes it.
   - `alloc_kernel_memory` stops building a `Vec<u8>` over a 2 MiB-aligned
     allocation. Both relocation unsafes go. `blackbox::Page` is not `Copy`, so
     `bytes()` mints no second `&'static mut`.
   - The layout word and every field before it keep their offsets, each
     pinned by a compile-time `offset_of!` assert, so moving `layout` fails to
     build. That prefix stays flat. After the word, `KernelArgs` is typed:
     `repr(C)` sub-structs, and `#[repr(C, u32)]` enums for the optional parts
     in place of `u32` presence flags and zero sentinels; a field the typing
     changes moves after the word. The slot booted and why (the one the table
     chose, or the other one after a refusal and its reason) and the black-box
     page are fields, not `boot-slot=`, `slot-refused=` and `blackbox=` text.
     `params.rs`' last-one-wins and kernel `main.rs`' branch for a format
     never emitted go. `kernel_stack_addr` is renamed for the offset it
     holds. x86's `_start` reads by `const offset_of!`.
   - The derived `LAYOUT` moves on its own once `size_of::<KernelArgs>()`
     does. `LAST_LAYOUT` pins stage 1's derived value, `0x5459_0000 | 1272`,
     as a literal, and the `loader-writes-the-last-layout` actuator makes the
     loader write it. A size change moves the word, and
     `kernel_args_last_layout_refused` catches it; a moved prefix field fails
     the prefix `offset_of!` asserts above; a change that keeps the size and
     lands after the word — a reorder, a type swap of equal size — moves
     neither, so every field after the word keeps its own `offset_of!` assert
     too, and `const _: () = assert!(LAYOUT != LAST_LAYOUT);` fails the build
     on a same-size stage until it bumps the word itself. This stage is an ABI
     change.
   - `toyos-update`'s slot table takes `toyos-gpt`'s CRC32 and loses its own.

   **Exit**: `loader_panic_powers_off` plants this key's floor in 9 bytes. It finds the
   handler's `loader: panicked at bootloader/src/` line in `loader.log`, which
   no other code writes, and QEMU reports `guest-shutdown`. It fails under
   the handler `bootloader/src/efi/mod.rs` holds before this stage, which
   writes no `loader.log`.
   `kernel_args_last_layout_refused` boots with
   `loader-writes-the-last-layout` and finds the kernel's refusal naming both
   words before any `black box:` record. Moving `layout` after
   `root_read_ticks` fails to build, and so does padding `KernelArgs` back to
   1272 bytes: `size_of::<KernelArgs>()` is then stage 1's size again, the
   derived `LAYOUT` collapses onto the literal `LAST_LAYOUT`
   (`0x5459_0000 | 1272`), and `assert!(LAYOUT != LAST_LAYOUT)` fails the
   build. The tests that read `KernelArgs` pass: `boot_from_power_on`,
   `bar_placement_is_proven`, `root_withheld_refused`, `blackbox_*` and
   `update_*`.

5. **Tries and a good flag per slot, decided on the host.** Each slot in the
   table carries a priority, the tries it has left and a good flag, in the
   word the format reserves; the table's format rises. They replace the
   attempts file (`attempt.rs`, `toyos-update/src/record.rs` and `main.rs`'
   accounting), the dead-slot retry and `slot::proven`.
   - A freshly built image's slot A starts untried with 3 tries. An install
     writes its slot untried with 3 tries, the good flag clear and a priority
     above the kept slot's, whatever the slot held before.
   - A pass boots the highest-priority slot that verifies and is good or has
     tries left. A slot out of tries and never good is not booted again.
   - Any failure after `verify` in `start_kernel` that the image causes is a
     refusal of that slot, like a bad signature, and the pass falls to the
     other slot. Stage 5's PR lists which `start_kernel` refusals count as the
     image's — every `load_kernel_elf` refusal of the kernel's bytes — and
     which count as the loader's own.
   - The loader spends a try of an untried slot before it hands over. That
     write, with sealing `ARMED`, is the pass's last, after every refusal
     `start_kernel` can make, so a loader refusal is never booked as the
     image's. Where the decrement cannot be persisted, that slot is not booted.
   - Where no slot can boot, the loader says so on the panel and in
     `loader.log`, and powers the machine off.
   - The health gate: `system.toml`'s `[boot] up` names the services that
     make the image up. The tree has no readiness signal (a swap's probation
     only asks whether a process still runs), so this stage adds the smallest
     one: init endows each named service the write end of a pipe under the
     label `ready`, and the service writes one byte to it once it serves. A
     read end that closes with no byte is a service that never came up.
   - Init runs `update --good` once every service `[boot] up` names has
     written its byte, and never for an image that names no `[boot] up`;
     `update` holds the `slots` claim's table and writes the running slot's
     good flag and nothing else.
   - An image whose `system.toml` grants no program the `slots` claim is never
     good, so each boot spends a try and it boots three times. Today only
     `system.toml` and `tests/updatecase/system.toml` grant it.
   - `update_refusals_boot_the_other_slot` boots slot A five times and
     `update_grant_refuses_a_stray_partition` four. Both run `updatecase`, and
     each boot waits for the good flag before the guest is dropped. Every
     other test hands one image's kernel at most three times.
   - The floor rises on a pass that boots a good slot, to the security version
     of the signed header that pass verified, never to a version the table
     names.
   - A one-shot request (`update --once`, `update --boot-first`) is cleared
     and saved before it is acted on, and ignored where the save fails.
   - `update --once` asks for the idle slot once and leaves it at priority 0,
     so its good flag is never read. The kept slot boots after it, and no floor
     rises for it. `KernelArgs`' slot field gains the once variant. The kernel
     reads the field's discriminant as a raw `u32` and refuses one it does not
     name, so an unrecognized value is a refusal rather than an out-of-range
     `#[repr(C, u32)]` read, which is UB. The once variant keeps the struct's
     size and every field's offset, so `size_of::<KernelArgs>()` alone would
     leave the derived `LAYOUT` sitting at stage 4's value; this stage instead
     bumps a layout-version component the formula also ORs in, so `LAYOUT`
     differs from `LAST_LAYOUT` (stage 4's derived value, pinned as a literal)
     and `assert!(LAYOUT != LAST_LAYOUT)` builds. This stage is an ABI change.
   - `update --boot-first` writes only the request. The loader writes its own
     `HD(…)/File(…)` entry and puts it first. Once stage 7 deletes
     `bootnext.rs`, that is the only boot-variable write.
   - This stage deletes the floor issue's "a floor that never rises" bullet
     and its exit's second half.

   The decisions are pure `toyos-update` functions:
   `verify(signed, kernel, cmdline, root, key, floor) -> Result<Verified, Refusal>`
   and `pass::decide(inputs) -> Decision` over the table, the verified slots,
   the floor, the request and whether the last write persisted. The loader does
   the I/O and carries the decision out.

   **Exit**: `decide`'s host tests hold each rule above, and `verify`'s hold
   each refusal; deleting the cmdline hash check or `policy::admits` fails a
   host test. The `update_*` tests and `hang_bounded_by_the_stick`, rewritten
   for tries, pass. The hang tests boot an image that never reaches its health
   gate, instead of killing QEMU on `LOADER_LAST_LINE`. #539's
   `update_boot_first_puts_the_loader_first` passes, and so do its host tests
   of the entry rules that survive.

   Negative controls:
   - Skipping the countdown fails `update_hang_kills_an_unproven_image`.
   - Raising the floor on any pass that boots a slot fails
     `update_floor_waits_for_good`. It installs into B an image at the running
     security version + 1 whose kernel hangs, signed by
     `tests/common/update.rs`' `Rig::update` at the version it is handed. B
     spends its tries and A boots, and `fwvars::live` reads the floor at A's
     version.
   - An install that leaves the good flag set, or its priority below the kept
     slot's, fails `update_over_a_good_slot_starts_it_untried`. It updates
     into B and boots B to good, then updates over A, which was good, with a
     kernel that hangs. The slot table read after each pass shows A's kernel
     booted three times, then B boots.
   - Raising the floor to the table's version fails
     `update_floor_is_the_headers`, which plants a table version above the
     header's and reads the floor at the header's.
   - `let persisted = true;` in the loader fails
     `update_readonly_stick_boots_nothing`. It boots a fresh image off a
     read-only stick: nothing boots, the no-bootable-slot line is on the
     console, and QEMU reports `guest-shutdown`.
   - Turning the refusal of an unloadable kernel back into a panic fails
     `update_unloadable_kernel_boots_the_other_slot`, which updates B with a
     signed non-ELF kernel, finds the loader's refusal of B naming a kernel
     that is not an ELF, and finds A booted.
   - Running `update --good` right after init spawns `[boot] start` fails
     `update_dead_service_is_never_good`. It updates B with an image whose
     `[boot] up` names a live service beside one that exits at start; the live
     service writes its byte and the dead one never does. B boots three
     times, its good flag never set because one named service never wrote,
     and then A boots.
   - Acting on a request before taking it off the table fails
     `update_boot_first_puts_the_loader_first`.

   Oracles: Android's A/B and Fuchsia libabr's rules, except that libabr boots
   a slot whose decrement failed; the floor `fwvars::live` reads out of the
   vars store; the slot table after each boot, decoded by the test from the
   format's bytes and not by `toyos_update::slots::Table::decode`; and OVMF's
   boot manager booting the entry the loader wrote.

6. **The T14 bench.**
   - It opens with a timed `ssh t14 update < image` of a session image of
     `issues/the-t14-reboots-through-ubuntu-for-every-test.md`, since
     an image switch costs that write and a reset: ToyOS has written this
     stick at 0.2 s to 9.5 s per MiB.
   - It is built from #539's pieces that stage 6 takes above, and the bench
     path of `src/metal.rs`. `--via-ubuntu` stays as the old path.
   - Every T14 image is signed with the bench key the T14 trusts, and a loader
     reaches the T14 as an image does: ToyOS receives it over ssh and writes it
     to the stick, and the running loader tries it once and keeps the old one
     as the fallback (owner rulings). A stick that boots neither loader costs
     a hand and `diag/flash.sh`.
   - The host's first exec on an image is `update --good`, and a session image
     names no `[boot] up`. A death goes by `update --once`, the kept slot boots
     after it, and the host fetches the record over `sftp`.
   - The bench reads the loader's file off the ESP.
   - The tested pass's `loader.log` is kept as `loader-previous.log` by a
     rename (`SetInfo`), not by #539's copy.
   - It runs over the cable that
     `issues/the-t14-answers-only-through-a-usb-stick.md` owns, and
     waits on
     `issues/a-reset-stops-xhci-and-leaves-every-claimed-pci-function-armed.md`,
     `issues/the-t14-hung-after-rebooting-with-its-i219-faulted.md`,
     `issues/a-fatal-event-stands-down-both-bounds-and-may-leave-nothing-to-end-the-machine.md`,
     `issues/most-t14-leases-land-one-dhcp-retry-late.md` and
     `issues/the-host-cannot-reach-the-t14-while-it-runs-toyos.md`,
     which deleted the host's reader, its ssh client and the machine's end of
     the stream, and records what they still owe.
   - #539's issues `a-loader-change-reaches-a-machine-only-by-writing-its-stick`,
     `the-bench-reads-no-quiescent-log-volume`,
     `the-bench-runs-with-no-bound-on-its-own-boot`,
     `the-bench-sometimes-comes-back-two-minutes-late` and
     `the-benchs-cable-is-read-by-the-driver-under-test` land here, each only
     as far as it is true of what lands.

   **Exit**: `bench_loop_drives_a_toyos_machine` passes in QEMU. On the T14,
   with Ubuntu never started: a whole run, its sessions and every boot that
   goes by `--once` (`deadlinewedge`, `hardlockup`, `usbload` and
   `foreignrecord` among them), gives the verdicts a per-boot run gave at the
   commit this stage branches from; a kernel change boots; a slot with a
   flipped byte, no signature or a lower security version is refused and the
   other boots; a slot that dies falls back on its own, and one whose `sshd`
   refuses the host's key is never marked good; a `--once` image that panics
   returns the machine to its session with its record judged; and a loader
   sent over ssh boots once, and one that brings no slot to good leaves the
   old one booting.

7. **Ubuntu leaves the loop.** Delete `toyos-metal`'s `--via-ubuntu` path,
   `--metal-via-ubuntu`, `bootloader/src/bootnext.rs` and test-runner's
   job-list mode. After a reset the firmware comes back to the loader because
   ToyOS's entry is first (stage 5).
   - `issues/a-hung-boots-log-partition-is-wiped-by-the-next-runs-flash.md`,
     `issues/the-sudoers-rendering-has-no-host-side-judge.md`,
     `issues/the-metal-loop-writes-the-readback-volume-into-a-directory-it-has-not-made.md`
     and `issues/the-t14-stopped-answering-ssh-between-two-lan-boots.md`
     close here, each only as far as it is true of what lands.

   **Exit**: no path in `src/metal*.rs` reaches Ubuntu. On the T14, a panic's
   reset reaches the loader with no `BootNext` set.

8. **The kernel arms the TCO before anything unbounded.** The loader's arm is
   the only bound today from `ExitBootServices` to `deadline::start` on
   `boot-deadline=` boots, and to `arch::watchdog::init` on the others:
   through `mm::init`, ACPI, interrupts, HPET calibration, the RTC read, PCI
   enumeration, `pcidev::publish` and `iommu::init`.
   - The kernel arms as its first act, before `mm::init`: `toyos_acpi::ecam_base`
     and the bus-0 scan `bootloader/src/watchdog.rs` does, through a boot map
     that maps bus 0's ECAM window. What stays unbounded is the loader's code
     from `ExitBootServices` to the jump.
   - The first feed comes a whole early boot after the arm. The kernel logs
     the span from arm to first feed, and it is measured under the bound on
     q35 and on the T14.
   - The kernel takes the read-back that
     `issues/an-armed-tco-has-never-reset-the-t14.md`'s exit needs:
     `TCO_RLD` at the arm and at the first feed (the counting read, with no
     stall), `TCO1_STS`, and `no_reboot`, `tco_lock` and `timeout`. That issue's
     exit reads the kernel's log in place of `loader.log`.
   - Deleted: `bootloader/src/watchdog.rs`, `arch::pio`, the kernel's
     on-arrival read with `ARMED_ON_ARRIVAL` and `UNARMED_ON_ARRIVAL`, and
     `tests/common/power.rs`' `loader_armed`, `armed_on_arrival` and
     `watchdog_quiet`'s loader half. `watchdog_armed` judges the kernel's
     read-back, and `loader_watchdog_arms` becomes the kernel's row.

   **Exit**: `bootloader/src` holds no TCO access. On the T14, `watchdog_armed`
   passes on the kernel's lines (counting, `no_reboot=0`, `timeout=0`). Once
   the reading of
   `issues/a-frozen-toyos-waits-for-a-hand-on-the-power-button.md` has
   seen the TCO reset the T14, a boot there that hangs right after the arm,
   before `mm::init`, is reset by the TCO; moving the arm back after
   `pci::enumerate` makes that row fail.

9. **The crash report belongs to the kernel.** The loader hands the last
   boot's record to the kernel instead of decoding it. The kernel logs it, and
   `logd` carries it to `/log`.
   - The kernel takes `harvest`'s check that a record belongs to the stick
     that wrote it (the log partition's GUID).
   - The kernel's report carries the record's byte count and
     `DROPPED_OPENS_WITH`'s count.
   - The report pass goes, taking with it `end_this_pass`, the chain
     constants, `loaderlog`'s chain lines, `armed_at`'s `GetTime`, everything
     else in `bootloader/src/blackbox.rs` except the claim and the arm.
   - `ENDS_AT_CHAIN` goes, so this stage rewrites the exit of
     `issues/a-wedged-reports-newest-records-were-cut-by-the-loaders-own-log-file.md`:
     a `deadlinewedge` rerun whose `/log` carries the `WEDGED` report with its
     byte count and drop line.

   **Exit**: `blackbox_panic_chain` reads the previous boot's panic out of
   `/log`, with none of it in `loader.log`, and every pass boots a kernel.
   Withholding the handover makes the test fail.
