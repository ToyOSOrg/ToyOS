---
status: open
kind: track
opened: 2026-09-03
---

# The T14 boots ToyOS unattended and reports through a log partition

The T14 stopped being a GitHub Actions runner (owner ruling, 2026-09-03), so
what the tracker owes "on hardware" is owed to this loop: a driver on this Mac
flashes the stick left plugged into the machine, sets one boot, reboots, and
reads the verdict off a log partition. **There is no serial channel** — the
16550 loopback reads `0xFF`
(`issues/hardware/a-metal-session-runs-a-pre-flash-gate-first.md`, which is also
the loop's admission check: no image is flashed that has not passed it) — so the
log partition and the screen are the only two channels there are.

What is left to build:

- **The chipset watchdog is armed and does not count**, so nothing this tree
  arms can end a wedged boot; a panicking kernel ends one by its own bound
  instead, and a kernel that wedges without panicking still ends nothing.
  `issues/hardware/an-armed-tco-has-never-reset-the-t14.md` carries the
  registers and the datasheet. **Exit**: a T14 boot that resets itself on an
  armed TCO.
- **An AP loads its IDT before its control registers**, so a fault in that span
  triple-faults the machine —
  `issues/kernel/an-ap-loads-the-idt-before-its-control-registers.md`, whose
  exit is one of the two orderings it names, judged by `smp_bringup` and the
  SMP suite.
- **A loader pass overwrites the pass before it.** Every pass writes
  `loader.log`, so on a chained boot the second pass's three lines replace the
  first pass's record of the boot it is reporting on — measured on run 10, where
  the surviving file was 228 bytes and the boot's own watchdog read-back was
  gone. **Exit**: a chained run whose stick carries every pass, each named by
  the pass that wrote it.
- **Thirteen instrument rows carry `UNMEASURED_MS`** and owe a price from the
  PR's own CI, with every name over `FAST_COMMIT_MS` moved to `Tier::Nightly`
  under a `Why::TimerAnchored` row in the commit after —
  `issues/build/the-metal-tracks-registrations-are-all-unmeasured.md`.
  **Exit**: no `UNMEASURED_MS` row left in `tests/test-durations`.
- **The measurements owed on hardware are this loop's jobs**, by record:
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
