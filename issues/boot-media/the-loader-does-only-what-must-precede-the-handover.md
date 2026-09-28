---
status: open
kind: track
opened: 2026-09-28
---

# The loader does only what must happen before the handover

About a third of `bootloader/src` loads no kernel. It serves the T14's
Ubuntu-hosted loop (`BootNext` back to the loader, a pass that boots nothing
and only reports, a reset into Ubuntu, which reads the stick) and jobs that
landed in the loader because the kernel never maps UEFI runtime services. The
owner's rulings (2026-09-28) set the bounds:

- the loader is slimmed;
- the hardware clock keeps UTC;
- no firmware code runs after `ExitBootServices` (root `CLAUDE.md`);
- ToyOS ends at least as secure as it starts.

The audit estimates, without measuring, that about 1,150 of the loader's 3,519
lines go and that `toyos-update/src/record.rs` (236 lines) becomes a few fields
of the slot table.

PR #539 does not land. Its pieces are placed in the stages below. Three of its
pieces are deleted:

- `update --boot-next <esp>`;
- the panic handler's fall to the next boot entry, together with the two issues
  it filed (`a-failed-pass-can-fall-to-an-entry-the-firmware-never-boots-from-its-order`
  and `the-loaders-power-off-with-no-entry-behind-it-is-proven-by-nothing`);
- the `bootvars::state` line.

Each stage lands on its own, in this order.

1. **Deletions that wait on nothing.**
   - Delete the loader's TCO arm (`bootloader/src/watchdog.rs`, `arch::pio`)
     and `loader_watchdog_arms`. This leaves the span from the jump to
     `arch::watchdog::init` with no bound on a machine whose TCO counts. On the
     T14 nothing bounds it today either
     (`issues/hardware/an-armed-tco-has-never-reset-the-t14.md`, whose evidence
     moves to the kernel's read-back).
   - Delete the time-zone read (`rtc_utc_offset`), `KernelArgs`' two zone
     fields, the `rtc_zone_east` actuator and `wall_clock_zone`. The kernel
     reads the RTC as UTC. Removing the fields is an ABI change and lands in
     this stage.
   - Delete `rootbridge.rs`'s hex dump.
   - Move `arch::counter_origin` (`IA32_TSC_ADJUST` on x86-64) into the
     kernel.
   - Delete the audit's ten worst comments where their files stay.

   **Exit**: `bootloader/src` holds no TCO access, no `counter_origin` and no
   zone. The Fast tier and `wall_clock_*` pass. Putting a zone back into
   `clock::init_wall` makes a `wall_clock_*` test fail.

2. **One HARDDRIVE rule.** #539's `toyos_update::entry::partition` is taken
   by `boot_partition`, `rootimage::boot_disk` and `bootnext.rs`.

   **Exit**: the loader loses lines. #539's host tests
   `a_path_names_the_partition_of_its_one_hard_drive_node` and
   `a_partitions_disk_is_the_path_before_its_hard_drive_node` pass, and fail
   under #539's two mutations: cutting the disk before the path's last node,
   and taking a second HARDDRIVE node. The oracle is UEFI 2.10 §10.3.5.1.

3. **One floor per key.** `floor::Scope` goes. Every loader keeps the floor
   `Scope::Machine` names, named for its key alone. `stale` and its deletions
   go.

   Why nothing is weaker:
   - The owner's floor keeps its name, attributes and judgement unchanged.
   - A throwaway key's floor is named today for the key and the log
     partition's GUID, so a write of that GUID resets it. After this stage,
     that write does not.
   - `stale` never deleted a loader's own floor, so removing it lowers no
     floor. What stays behind is one variable per key that has booted the
     machine, and only a loader, before the handover, writes one.

   The cost: a machine refuses an image older than one its key has already
   booted there, as it already does for the owner's key.

   **Exit**: the guest tests run the scope the owner's machine runs.
   `update_floor_is_the_images_own` is rewritten to check two things: this
   key's floor, planted above the image's version, refuses the image under a
   fresh log GUID; and another key's floor holds the image to nothing and
   stays. Putting the log GUID back into the name makes the test fail.

4. **Tries and a good flag per slot in the slot table.**
   - They replace the attempts file (`attempt.rs`, `toyos-update/src/record.rs`
     and `main.rs`' accounting), the dead-slot retry and `slot::proven`.
   - The loader counts a slot's tries down before it hands over, and boots no
     slot that is out of tries and was never marked good. The running system
     marks its own slot good.
   - The floor rises on a pass that boots a good slot, to the version of the
     signed header that pass verified and never to a version the table names.
     That meets the second half of the exit in
     `issues/boot-media/the-anti-rollback-floor-is-a-firmware-variable.md`.
   - `update --once` sets tries = 1 on the idle slot, and that slot is never
     marked good. The kept slot boots next, and no floor rises.
   - `update --boot-first` is the only boot-variable write. The loader writes
     its own `HD(…)/File(…)` entry and puts it first, once, from a request it
     takes off the table before acting on it.

   **Exit**: the `update_*` guest tests, rewritten for tries, pass.
   `update_falls_back_from_a_dying_kernel` and
   `update_hang_kills_an_unproven_image` hold as they do today. #539's
   `update_boot_first_puts_the_loader_first` passes, and so do its host tests
   of the entry rules that survive.

   Negative controls:
   - Skipping the countdown makes `update_hang_kills_an_unproven_image` fail.
   - Raising the floor on a slot not yet good makes an `update_*` test fail.
   - Acting on the request before taking it off the table makes
     `update_boot_first_puts_the_loader_first` fail.

   Oracles: the slot table the host reads off the disk after each boot, and
   OVMF's boot manager booting the entry the loader wrote.

5. **The T14 bench.**
   - It is built from #539's `src/metalbench.rs`, the bench path of
     `src/metal.rs`, `tests/common/bench.rs`, `tests/bench*case` and
     `--bench-image`. `--via-ubuntu` stays as the old path.
   - The loader names no hash of its own file. The bench reads the file off
     the ESP.
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
   with a flipped byte, no signature or a lower version is refused and the
   other boots; and a boot that dies falls back on its own.

6. **Ubuntu leaves the loop.** Delete `toyos-metal`'s `--via-ubuntu` path and
   `bootloader/src/bootnext.rs`. After a reset the firmware comes back to the
   loader because ToyOS's entry is first (stage 4).

   **Exit**: no path in `src/metal*.rs` reaches Ubuntu. On the T14, a panic's
   reset reaches the loader with no `BootNext` set.

7. **The crash report belongs to the kernel.** The loader hands the last
   boot's record to the kernel instead of decoding it. The kernel logs it, and
   `logd` carries it to `/log`. The report pass goes, taking with it:
   - `end_this_pass`;
   - the chain constants;
   - `loaderlog`'s chain lines;
   - `armed_at`'s `GetTime`;
   - everything else in `bootloader/src/blackbox.rs` except the claim and the
     arm;
   - the chained-pass item of `issues/hardware/the-t14-boots-toyos-unattended.md`.

   **Exit**: `blackbox_panic_chain` reads the previous boot's panic out of
   `/log`, with none of it in `loader.log`, and every pass boots a kernel.
   Withholding the handover makes the test fail.
