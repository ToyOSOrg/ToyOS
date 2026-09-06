---
status: open
kind: track
opened: 2026-09-03
---

# The T14 boots ToyOS unattended and reports through a log partition

The T14 stopped being a GitHub Actions runner (owner ruling, 2026-09-03). What
the tracker owes "on hardware" moves to this loop: a driver on this Mac reaches
the machine over Tailscale, flashes an image to the USB stick left plugged into
it, sets one boot, reboots, and reads the verdict back off a partition. **There
is no serial channel**: the 16550 loopback reads `0xFF`
(`issues/hardware/a-metal-session-runs-a-pre-flash-gate-first.md`), so the log
partition and the screen are the only two channels there are, and the loop is
built on the first.

## Stages

1. **The driver**, a small Rust program run on this Mac, no Python: assert the
   pre-flash gate on the image, assert the stick's identity over SSH — **the
   internal NVMe is never written, mounted or stamped** — flash it,
   set `efibootmgr --bootnext` so a failure to come back is one reboot and not a
   machine stuck in ToyOS, reboot, wait, mount the log partition, and answer
   pass or fail from the log's verdict line. Every SSH command is one of a fixed
   list and the sudoers rule permits exactly that list. The harness's `metal`
   profile is its consumer.
2. **A chipset watchdog**, so a wedged kernel is reset without a hand on the
   power button. The T14 is Tiger Lake-LP — LPC `8086:a082`, SMBus `8086:a0a3` —
   and Linux 6.8.0-138's `lpc_ich` claims neither id among its 237 PCI aliases:
   the TCO block is reached through the SMBus controller's TCOBASE, which is why
   `i2c_i801` is the module claiming `8086:a0a3` here. QEMU's q35 models ICH9's
   PMBASE+`0x60` block instead (`hw/acpi/ich9_tco.c`,
   `include/hw/southbridge/ich9.h:205`), so the register reference is Intel's
   Tiger Lake-LP PCH datasheet and the judge is the machine, which carries no
   watchdog today — no `/sys/class/watchdog`, no WDAT among the firmware's
   tables. The budget is five minutes. **The milestone does not wait on it**: a
   finished run reboots itself.
3. **The measurements owed on hardware become its jobs**, by record:
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
4. **The first milestone is one unattended boot with a log back** — flashed,
   booted, verdict read, machine returned to Linux, nobody in the room.
   The 2026-09-06 attempt stopped with the loader's `Applied 4275 relocations`
   still the last thing on the panel and TOYOS-LOG empty, so what is built next
   is the `early-panel` boot parameter — every kernel record repaints the panel
   until the first `boot_phase!` — and a loader that builds its page tables
   before `ExitBootServices`, where a refusal can still print.

`src/bootlog.rs` holds the reset word the verdict looks for, and the kernel
spells it a second time at `kernel/src/arch/syscall/machine.rs`'s `quiesce`:
the only crate a `no_std` kernel and the host both read is `toyos-abi`, whose
sources land alone. Naming it there is the exit.
