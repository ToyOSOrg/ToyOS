---
status: open
kind: track
opened: 2026-09-03
---

# The T14 boots ToyOS unattended and reports through a log partition

The T14 stopped being a GitHub Actions runner (owner ruling, 2026-09-03). What
the tracker owes "on hardware" moves to this loop: a driver on the machine's
Linux side flashes an image to the machine's own ToyOS partitions, sets one
boot, reboots, and reads the verdict back off a partition. **There is no serial
channel**: the 16550 loopback reads `0xFF`
(`issues/hardware/a-metal-session-runs-a-pre-flash-gate-first.md`), so the log
partition and the screen are the only two channels there are, and the loop is
built on the first.

## Stages

1. **The T14 dual-boots.** Linux keeps the disk and runs the loop's driver;
   ToyOS gets ESP entries and its own partitions, in the shape
   `issues/filesystem/storage-is-layers-and-a-role-is-a-filesystem.md` gives
   Block and Volume. Blocked on that track's root filesystem PR 1 — the
   internal-disk boot — which is in flight.
2. **One boot into ToyOS, set from Linux** (`efibootmgr --bootnext`, so a
   failure to come back is one reboot and not a machine stuck in ToyOS). ToyOS
   runs its job, writes the log to the log partition, and reboots. **The
   reboot path does not exist**: `kernel/src/drivers/acpi.rs`'s `shutdown` is
   ACPI S5 through `PM1a_CNT` and there is no reset path in this kernel — no
   FADT reset register, no other. Something that returns the machine to the
   firmware is stage 2's own work.
3. **The driver**, a small Rust program on the Linux side, no Python: flash the
   image to the ToyOS partitions, set bootnext, reboot, wait, mount the log
   partition, and answer pass or fail from the log's verdict line. The
   harness's `metal` profile is its consumer.
4. **The measurements owed on hardware become its jobs**, by record:
   `issues/kernel/the-split-window-tlb-cost-is-unpriced.md`,
   `issues/kernel/ap-control-registers-inherit-init.md`,
   `issues/kernel/ap-tsc-trail-is-assumed-and-never-checked.md`,
   `issues/audio/hda-ring-fix-unverified-on-metal.md`,
   `issues/audio/t14-wake-lateness-is-bimodal-per-boot.md`,
   `issues/audio/gate-a-has-no-runner-baseline.md` (a metal sample), and the
   IOMMU track's three hardware-only answers — isolation scopes and reserved
   regions, the 2× cost bar, and the compatibility-format question in
   `issues/kernel/qemu-passes-compatibility-format-interrupts.md`
   (`issues/kernel/the-iommu-refuses-nothing-yet.md` states the first two).
   `issues/hardware/a-metal-session-runs-a-pre-flash-gate-first.md` is the
   loop's admission check: no image is flashed that has not passed it.
5. **The first milestone is one unattended boot with a log back** — flashed,
   booted, verdict read, machine returned to Linux, nobody in the room.
