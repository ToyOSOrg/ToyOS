---
status: open
kind: defect
opened: 2026-08-08
---

# Metal boot is 1151 ms against QEMU's 196 ms, and the recorded accounting for it is stale

**The numbers, taken out of the 2026-08-07 freeze capture rather than
re-measured.** Six of its healthy boots report `Boot: complete` at 1148, 1148,
1149, 1150, 1151 and 1154 ms; the seventh is 755 ms and is the control boot whose
keyboard was refused, so its peripherals phase is 448 ms instead of 842. The QEMU
figure for the comparable shape is `Boot: complete (196ms)` on the
`metal_sim_compositor` boot
(`kernel-log-unreadable-once-userland-owns-the-screen` records the
measurement), and `(234ms)` for the diag artifact booted
headless. **So metal is ~5.9× QEMU, not the ~17× the hardware inventory
computed** — that ratio is against `(3422ms)`, and 2.30 s of those 3.42 s were
the six `boot_checkpoint` framebuffer repaints, which #138's write-combining
change removed. Measuring the phase-boundary gaps in the longest of the seven
myself, all six together are **73 ms** against that boot's 2308. Any metal boot
timing carried forward from that inventory describes a machine that no longer
exists.

**Where the 1151 ms goes now**, from that same boot:

| phase | reported |
|---|---|
| CPU ready | 60 ms |
| storage ready | 84 ms |
| **peripherals ready** | **842 ms** |
| subsystems ready | 93 ms |
| devices ready | 20 ms |

Peripherals is 73% of the boot, and its two largest components are:

- **393 ms of i8042 keyboard init** — `i8042: ok selftest=0x55` at 0.609, the
  next i8042 line at 1.002. Real hardware, not a probe of an absent one.
- **206 ms establishing the Thunderbolt xHC at `00:0d.0` has nothing on any
  port** — `controller started` at 0.161, `no HID devices on the controller at
  00:0d.0` at 0.367. Four of the PCH's port resets are 55 ms each, which is USB's
  own and not the driver's to shorten.

**Absent-device probing is ~279 ms of 1151, not ~1.1 s.** The other piece is the
PCI walk: `PCI: Enumerating devices...` at 0.065, last real function `0a:00.0` at
0.072, `Enumeration complete, 24 functions.` at 0.145 — **73 ms** scanning buses
that hold nothing, against 7 ms finding everything that is there.

**What this entry is for.** Metal boot time has no owner of its own; the only
accounting it ever had was written against the superseded 3422 ms boot and
pointed at "#65 (boot time)" as its owner. Whatever #65 says, its numbers
should come from this table:
the two-thirds that motivated it were paints and are gone. Note also the NIC
retry that looks like boot cost and is not — `toyos/src/net.rs:271`'s 100 retries
at 10 ms run *after* `Boot: complete` (see *Every network client pays a second of
boot retry on a machine with no NIC*), and `READY_BUDGET_NS` bounds retries
rather than boot time (`issues/filesystem/`).

## Promoted 2026-08-25

A live, actionable measurement table for boot-time task #65 to consume,
correcting a stale ~17x figure another finding still cites as its owner's
number. Owed to whoever picks up #65.

Carried here when the scanout-price entry closed (#342), so
the panic-console thread keeps its facts: the T14 measured 461/459 ms repaints
on the panic path; the open painter-granularity question — a glyph assembled in
a scratch row and blitted as one run would merge where the per-bit
`write_volatile` stores do not — and the constraint that rules out the obvious
fix: the panic path takes no lock, so a shared static scratch strip re-creates
the multi-CPU race issues/panic-path/ records against capture().
