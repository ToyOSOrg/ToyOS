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
  instead, and a kernel that wedges without panicking after `clock::init` is now
  ended by `kernel/src/deadline.rs` (below) — before it, still nothing.
  `issues/hardware/an-armed-tco-has-never-reset-the-t14.md` carries the
  registers and the datasheet. **Exit**: a T14 boot that resets itself on an
  armed TCO.

  **The software answer was designed and is not built, and the first reason is
  a number this loop does not have yet.** The design evaluated was a sentinel:
  an AP started early becomes a CPU spinning on the TSC, the BSP publishes each
  boot phase it reaches, and a phase missed within a bound seals a record into
  the black box and writes the FADT reset register. Four findings, taken by
  reading the code:

  1. **The bound cannot be derived.** It has to be wider than the slowest
     healthy phase on this machine and narrower than a wait for a hand, and
     nothing in this tree has measured a T14 phase duration. A bound guessed
     wrong resets a healthy laptop in a loop, which is worse than the present
     state. This loop's boot-fact run is what publishes those numbers, so the
     design is sequenced behind it rather than blocked on a ruling.
  2. **A sentinel is a CPU outside the roster, and the roster is modelled.**
     `Roster::begin_attempt`/`commit` hands out dense ids that `boot_aps` fills
     in attempt order, `smp_failed_ap_leaves_no_hole` is the gate that exists
     because a hole in them is a defect, and `kernel-loom/tests/smp_bringup.rs`
     is what decides whether a second committer is sound. A CPU taking an id
     before `boot_aps` runs is a change to that protocol and needs the model
     extended first.
  3. **It cannot start before the machine has a clock without a second
     bring-up path.** `boot_aps`' SDM §8.4.4.1 delays and its 100 ms start
     budget are spun on `clock::nanos_since_boot`, which answers zero until
     `clock::init` — so an AP started before that never leaves the delay.
     `clock::cpuid_tsc_hz` exists for the panic path's version of this problem
     and would serve, at the cost of a second decider for how long an
     INIT-SIPI wait is.
  4. **The reset it would take is not lock-free.** `acpi::reboot` opens with
     `serial::flush_final`, and a `BackendGuard` masks interrupts for its whole
     life — so a BSP wedged inside one holds the lock a sentinel would spin on,
     on exactly the boots the sentinel exists for. A `reset_now` that writes the
     decoded port and nothing else is small and separable from the rest.

  What no design of this shape can cover is the span before `apic::init`: no AP
  can be started before the BSP's own LAPIC is enabled, and `acpi::init_reset`
  — which decodes the register any of this would write — runs inside it. That
  floor is inherent rather than an argument against the design.

  **The half of it that does not need an AP is built** (`kernel/src/deadline.rs`):
  a bound armed off the parameter line as `boot-deadline=<ms>` and polled from
  the timer interrupt entry, in both rings, on every CPU — two atomics and a
  `rdtsc`, no lock, no allocation. On expiry it seals a `WEDGED` record naming
  the bound, the uptime, the boot phase and **the tail of the log ring** into
  the black box, and writes the reset register. That closes findings 1 and 4
  for everything after `clock::init`: the bound is derived rather than guessed
  (`toyos_tco::WEDGE_BOUND_MS`, twice the runner's own, so a job the runner is
  about to end is not a wedge), and `acpi::reset_now` is the lock-free write
  finding 4 asked for — `reboot` now goes through it, so there is one writer of
  that port. It ends a wedge whose every CPU has stopped taking scheduler
  passes: measured on QEMU as `boot_deadline_ends_a_wedge`, 18 s with the arm
  against a machine that never came back without it, twice at 66 s.

  Findings 2 and 3 are untouched and are exactly what is left: they are about a
  CPU outside the roster, and the only thing that needs one is the span this
  cannot reach — before `clock::init`, and a machine on which no CPU takes an
  interrupt at all. `kernel/src/deadline.rs`'s header states both as the same
  seam. Nothing here is a second mechanism beside the sentinel; it is the
  sentinel's seal, bound and reset, waiting for its CPU.
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
- **A timer-anchored name's tier is decided by its price**, not by the
  classification `FAST_CEILING_MS` states, so two of this track's names sit Fast
  on a verdict a slower machine moves —
  `issues/build/a-timer-anchored-names-tier-is-decided-by-its-price.md`, whose
  exit is the owner's answer on which of the two the boundary is.
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
