---
status: open
kind: track
opened: 2026-09-28
---

# The loader does only what must happen before the handover

The owner's bounds:

- the loader does only what must precede the handover;
- the hardware clock keeps UTC;
- ToyOS calls no UEFI runtime service (root `CLAUDE.md`);
- ToyOS ends at least as secure as it starts;
- the anti-rollback floor counts a signed security version, raised only by a
  release that fixes a security hole.

PR #539 does not land. Its pieces:

- stage 2 takes `toyos_update::entry::partition` and its two host tests;
- stage 5 takes the rest of `toyos_update::entry` and its host tests,
  `update --boot-first`, `update --once` and
  `update_boot_first_puts_the_loader_first`;
- stage 6 takes the bench: `src/metalbench.rs`, `tests/common/bench.rs`,
  `tests/bench*case`, `--bench-image`, toybox `date`, `Ssh::probe` and
  `Ssh::fetch` with `ssh-client-host`'s `probe` and `fetch`,
  `build::AUTHORIZED_ON_ROOT`, and `src/bootlog.rs`' `LOADER_PREVIOUS_LOG`,
  `MOUNTED_FROM_MEMORY` and `BOOT_PARAMETER`;
- deleted: `update --boot-next <esp>` with `Guid::parse`, its only reader; the
  panic handler's fall to the next boot entry, with the two issues it filed
  (`a-failed-pass-can-fall-to-an-entry-the-firmware-never-boots-from-its-order`
  and `the-loaders-power-off-with-no-entry-behind-it-is-proven-by-nothing`);
  the `bootvars::state` line; `SLOT_ONCE`, because stage 4's `KernelArgs`
  carries a once boot as a field; `LOADER_IS`, because the loader names no hash
  of its own file.

Each stage lands on its own, in this order.

1. **What PR #583 lands.**
   - The RTC keeps UTC. The loader's `GetTime` zone read, `KernelArgs`' two
     zone fields, the kernel's `UTC_OFFSET_SECS` and its local/UTC split, the
     `rtc-zone-east` actuator, `wall_clock_zone` and logd's zone recovery go.
     FAT stamps, `SYS_CLOCK_REALTIME` and `SYS_CLOCK_EPOCH` read
     `clock::utc_secs`.
   - `rootbridge.rs`' hex dump goes, with `toyos-acpi`'s `Walk::bytes`.
   - Ten loader and `toyos-update` comments go, each named in #583's commits.
   - `KernelArgs` carries a layout identity the kernel checks first. `update`
     never replaces the ESP loader, so a slot's kernel can meet a loader built
     to another layout; it is refused by name.
   - The kernel reads `IA32_TSC_ADJUST` beside its power-on report. aarch64's
     `counter_origin` is deleted: the kernel reads `CNTFRQ_EL0` itself.
   - The loader's TCO arm stays until stage 8.
   - This stage is an ABI change.

   **Exit**: `bootloader/src` holds no zone and no `counter_origin`.
   `wall_clock_utc` judges the log file's name, its FAT stamp, the probe's
   `local=` and `SYS_CLOCK_EPOCH` against the `-rtc base=` instant. It fails
   under a kernel that reads the RTC as local time two hours east, and under
   `SYS_CLOCK_REALTIME` patched to `utc_secs + 7200`. A mixed pair, the loader
   at the base and the kernel at the head, is refused by the identity check's
   name. `boot_from_power_on` fails where the kernel's `IA32_TSC_ADJUST` line is
   missing.

2. **One boot disk.**
   - `toyos_update::entry::partition` is the one HARDDRIVE rule, taken by
     `boot_partition`, `rootimage::boot_disk` and `bootnext.rs`.
   - Every volume the loader opens (the slot's FAT partition, the log
     partition and the attempts file on it) is found on the boot disk, where
     exactly one match is taken. `loaderlog::volume_handle`'s machine-wide
     first match goes.
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
   - The signed header's `version` is the security version: a number the tree
     holds, raised only by the change that fixes a security hole.
     `image::version_now` goes. The header's `FORMAT` rises, so a header
     carrying a build time is refused by name.
   - `floor::Scope` goes. Every loader keeps one floor per signing key, named
     for the key alone and under a name no build-time floor carries. `stale`
     and its deletions go.
   - No loader deletes another key's floor, because a deletion lowers it. The
     store keeps one 8-byte variable per key that has booted the machine.
   - `policy::installable` admits an image at or above the running image's
     and the idle slot's security version.
   - This stage deletes the throwaway-key bullet of
     `issues/boot-media/the-anti-rollback-floor-is-a-firmware-variable.md` and
     the per-image floor in stage 1 of
     `issues/boot-media/the-machine-updates-itself-without-ubuntu.md`. The floor
     issue's TPM 2.0 NV counter stays its exit: `TPM2_NV_Increment` moves one
     step, and so does a security version.

   Why nothing is weaker:
   - The floor refuses every image below the highest security version a boot
     has proven, which is every image the owner has declared holed.
   - A throwaway key's floor no longer resets when the log partition's GUID
     is written.
   - Only a loader, before the handover, writes a floor.

   **Exit**: the guest tests run the scope the owner's machine runs.
   `update_floor_is_the_images_own` checks three things. This key's floor,
   planted above the image's security version, refuses the image under a
   fresh log GUID. An older build at the floor's security version boots.
   Another key's floor holds the image to nothing and stays. Putting the log
   GUID back into the name makes it fail. The oracle is the vars store as the
   host reads it (`vars::live` in `tests/common/update.rs`): each floor's name
   and its UEFI 2.10 §8.2 attributes.

4. **The loader on current `uefi`, sound, with a typed handover.**
   - `uefi` and `uefi-raw` move to their current releases, and `uefi-services`
     goes. The unsafe `BlockIO` media cast in `rootimage.rs` goes with the old
     layout.
   - The loader's own `#[panic_handler]` writes the panic through `loaderlog`,
     then resets the machine.
   - `alloc_kernel_memory` stops building a `Vec<u8>` over a 2 MiB-aligned
     allocation. Both relocation unsafes go. `blackbox::Page` is not `Copy`, so
     `bytes()` mints no second `&'static mut`.
   - `KernelArgs` is typed: `repr(C)` sub-structs, and `#[repr(C, u32)]` enums
     for the optional parts in place of `u32` presence flags and zero
     sentinels. The slot booted, the slot refused and why, and the black-box
     page are fields, not `boot-slot=`, `slot-refused=` and `blackbox=` text;
     `params.rs`' last-one-wins and kernel `main.rs`' branch for a format never
     emitted go. `kernel_stack_addr` is renamed for the offset it holds, which
     closes `issues/kernel/the-kernel-reserves-its-stack-offset-as-a-physical-region.md`.
     x86's `_start` reads by `const offset_of!`, and the hand offset asserts
     go. The layout identity changes. This stage is an ABI change.
   - `toyos-update`'s slot table takes `toyos-gpt`'s CRC32 and loses its own.

   **Exit**: `bootloader/Cargo.toml` names no `uefi-services`. A guest test
   boots an image whose signed kernel is no ELF, and finds the loader's panic
   in `loader.log` and a reset; under `uefi::helpers`' handler it fails. The
   tests that read `KernelArgs` pass: `boot_from_power_on`,
   `bar_placement_is_proven`, `root_withheld_refused`, `blackbox_*` and
   `update_*`.

5. **Tries and a good flag per slot, decided on the host.** Each slot in the
   table carries a priority, the tries it has left and a good flag, in the
   word the format reserves; the table's format rises. They replace the
   attempts file (`attempt.rs`, `toyos-update/src/record.rs` and `main.rs`'
   accounting), the dead-slot retry and `slot::proven`.
   - A freshly built image's slot A starts untried with 3 tries. An install
     writes its slot untried with 3 tries and the good flag clear, whatever
     the slot held before.
   - A pass boots the highest-priority slot that verifies and is good or has
     tries left. A slot out of tries and never good is not booted again.
   - The loader spends a try of an untried slot before it hands over. That
     write, with sealing `ARMED`, is the pass's last, after every refusal
     `start_kernel` can make, so a loader refusal is never booked as the
     image's. Where the decrement cannot be persisted, that slot is not booted.
   - Where no slot can boot, the loader says so on the panel and in
     `loader.log`, and powers the machine off.
   - The running system marks its slot good once the image's health gate is
     up: the servers its `system.toml` names, which for the shipping image are
     `sshd` and `update`'s claims. Init starting is not the gate.
   - The floor rises on a pass that boots a good slot, to the security version
     of the signed header that pass verified, never to a version the table
     names.
   - A one-shot request (`update --once`, `update --boot-first`) is cleared
     and saved before it is acted on, and ignored where the save fails.
   - `update --once` asks for the idle slot once and leaves it at priority 0,
     so its good flag is never read. The kept slot boots after it, and no floor
     rises for it.
   - `update --boot-first` writes the loader's own `HD(…)/File(…)` entry and
     puts it first. Once stage 7 deletes `bootnext.rs`, it is the only
     boot-variable write.

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
   - Raising the floor on a slot not yet good fails an `update_*` test.
   - An install that leaves the good flag set fails
     `update_over_a_good_slot_starts_it_untried`. It updates into B and boots B
     to good, then updates over A, which was good, with a kernel that hangs; A
     spends its tries and B boots.
   - Raising the floor to the table's version fails
     `update_floor_is_the_headers`, which plants a table version above the
     header's and reads the floor at the header's.
   - Booting after a failed decrement fails a host test of `decide`.
   - Acting on a request before taking it off the table fails
     `update_boot_first_puts_the_loader_first`.

   Oracles: Android's A/B and Fuchsia libabr's rules, which these are; the
   floor the host reads out of the vars store; the slot table the host reads
   off the disk after each boot; and OVMF's boot manager booting the entry the
   loader wrote.

6. **The T14 bench.**
   - It is built from #539's pieces that stage 6 takes above, and the bench
     path of `src/metal.rs`. `--via-ubuntu` stays as the old path.
   - The bench reads the loader's file off the ESP.
   - The tested pass's `loader.log` is kept as `loader-previous.log` by a
     rename (`SetInfo`), not by #539's copy.
   - It runs over the cable that
     `issues/hardware/the-t14-answers-only-through-a-usb-stick.md` owns.
   - #539's issues `a-loader-change-reaches-a-machine-only-by-writing-its-stick`,
     `the-bench-reads-no-quiescent-log-volume`,
     `the-bench-runs-with-no-bound-on-its-own-boot`,
     `the-bench-sometimes-comes-back-two-minutes-late` and
     `the-benchs-cable-is-read-by-the-driver-under-test` land here, each only
     as far as it is true of what lands.

   **Exit**: `bench_loop_drives_a_toyos_machine` passes in QEMU. On the T14,
   with Ubuntu never started, three things hold: a kernel change boots; a slot
   with a flipped byte, no signature or a lower security version is refused
   and the other boots; and a slot that dies falls back on its own.

7. **Ubuntu leaves the loop.** Delete `toyos-metal`'s `--via-ubuntu` path and
   `bootloader/src/bootnext.rs`. After a reset the firmware comes back to the
   loader because ToyOS's entry is first (stage 5).

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
     `issues/hardware/an-armed-tco-has-never-reset-the-t14.md`'s exit needs:
     `TCO_RLD` at the arm and at the first feed (the counting read, with no
     stall), `TCO1_STS`, and `no_reboot`, `tco_lock` and `timeout`. That issue's
     exit reads the kernel's log in place of `loader.log`.
   - Deleted: `bootloader/src/watchdog.rs`, `arch::pio`, the kernel's
     on-arrival read with `ARMED_ON_ARRIVAL` and `UNARMED_ON_ARRIVAL`, and
     `tests/common/power.rs`' `loader_armed`, `armed_on_arrival` and
     `watchdog_quiet`'s loader half. `watchdog_armed` judges the kernel's
     read-back, and `loader_watchdog_arms` becomes the kernel's row.

   **Exit**: `bootloader/src` holds no TCO access. On q35, `watchdog_armed`
   passes on the kernel's lines (counting, `no_reboot=0`, `timeout=0`), and
   `watchdog_resets` passes. A boot that hangs right after the arm, before
   `mm::init`, is reset by the TCO; moving the arm back after `pci::enumerate`
   makes that test fail.

9. **The crash report belongs to the kernel.** The loader hands the last
   boot's record to the kernel instead of decoding it. The kernel logs it, and
   `logd` carries it to `/log`.
   - The kernel takes `harvest`'s check that a record belongs to the stick
     that wrote it (the log partition's GUID).
   - The kernel's report carries the record's byte count and
     `DROPPED_OPENS_WITH`'s count.
   - The report pass goes, taking with it `end_this_pass`, the chain
     constants, `loaderlog`'s chain lines, `armed_at`'s `GetTime`, everything
     else in `bootloader/src/blackbox.rs` except the claim and the arm, and the
     chained-pass item of `issues/hardware/the-t14-boots-toyos-unattended.md`.
   - `ENDS_AT_CHAIN` goes, so this stage rewrites the exit of
     `issues/diagnostics/a-wedged-reports-newest-records-were-cut-by-the-loaders-own-log-file.md`:
     a `deadlinewedge` rerun whose `/log` carries the `WEDGED` report with its
     byte count and drop line.

   **Exit**: `blackbox_panic_chain` reads the previous boot's panic out of
   `/log`, with none of it in `loader.log`, and every pass boots a kernel.
   Withholding the handover makes the test fail.
