---
status: assigned
kind: defect
opened: 2026-10-31
---

# The T14's idle package stays in PC2

Held by the power task that follows #1100.

On the T14, `osbench_energy` gives an idle package of 7137 mW under ToyOS
against 1648 mW under Linux, 4.3×: the median, over four runs a side, of
each run's per-second p50. The medians of the runs' means are 8272 against
2167 mW, 3.8×. Under a 10 ms sleeper it is 2863 against
1219 mW. ToyOS's package spends 994 ‰ of the idle minute in PC2 and none
deeper, against Linux's 102 ‰ in PC2. Every ToyOS core waits in C7. Power
belongs in the idle states and in device power management: none of this gap
may be closed by slowing work. The AMD bench's smaller gap is
`issues/the-amd-benchs-idle-package-draws-15-percent-over-linuxs.md`.

Every Linux figure here had its display on as software holds it: the eDP
connector's DPMS On, and the backlight at its setting. The one exception is
the display-off step. The lid's physical state before 2026-11-01 is unknown;
it has been closed since then. The ToyOS-against-Linux figures therefore hold
for one lid state, whichever it was (evidence `pr-1131` `lid/`).

Intel's datasheet (631121 Vol. 1 §3.2.5, Table 3-5) holds the package in PC2
when its cores are in C6 or deeper and its graphics in RC6, but an LTR, a
near timer or a device memory access refuses any deeper state. Table 3-8
caps the package at PC3 while a processor PCIe link is in L0, and at PC8
until it reaches L1.2.

Ruled out, by measurement:

- **The driverless functions.** Parking all nine in D3hot changed nothing
  under ToyOS, and unbinding them under Linux cost Linux nothing.
- **The deepest core state.** Holding every core at an 800 µs wake latency
  is inside the idle seconds' scatter.
- **The cores and graphics as consumers.** PP0 is about 0.3 W and PP1 under
  0.03 W, so about 6.8 W lies outside both planes. On Linux the equivalent
  is about 1.5 W.
- **The I219.** Parking it under ToyOS, or unbinding it under Linux,
  changed nothing.

The two causes, each reproduced on Linux by putting ToyOS's state there:

- **The NVMe without APST** (`issues/diskserver-drives-nvme-apst.md`).
  - With APST off, Linux's idle rises from 1622 to 5127 mW (per-second p50), and its package goes no deeper than PC3.
  - The rise is large when nothing wakes and smaller under a 10 ms sleeper, which is ToyOS's own shape.
  - The link's ASPM, L1 substates and LTR are as firmware left them, the same on both systems.
- **The display as the GOP left it.**
  - With no graphics driver (`nomodeset`), Linux idles at 2796 mW and stays in PC2 976 ‰. Its 10 ms sleeper draws 2864 mW, against ToyOS's 2831.
  - Under ToyOS, stopping the scan-out's plane fetches (Intel's PLANE_CTL Plane Enable) releases PC2 for PC3.
  - Linux's driver reaches PC8 with the panel lit. The fix is ToyOS's own display programming, with the panel lit; turning it off is not the fix. Per Intel's Tiger Lake display reference (Vol. 12), that means watermarks, SAGV, DMC firmware for DC5/DC6, and PSR for PC9/PC10. A driver for it is an owner decision.

Under ToyOS, APST did not lower the package (PR #1131, evidence `pr-1131`
r1-r3).
- The drive reaches PS4 between I/Os, yet the package stays in PC2 at 6-7 W
  either way, so the display holds it.
- Two things remain unexplained, with or without APST:
  - **The baseline.** About 6.3 W at the per-second p25, against Linux
    `nomodeset`'s 2.8 W in the same PC2.
  - **The excursions.** 2-20 seconds a minute at or above each run's p25 + 4 W,
    against Linux's 1-2 (4 under `nomodeset`). Every core is in C7 through
    them, so they are not CPU work.
- **The excursions follow no software-visible event** (r4).
  - Each idle second was set against the kernel's diary and the drive's I/O:
    which tasks woke, timer fires, idle entries, each CPU's C0 and every NVMe
    command.
  - No candidate was present in at least 80% of excursion seconds and at most
    30% of the others. NVMe commands fell in 9% of them.
  - The drive's states may raise their rate. With the drive reaching PS4
    400 ms after I/O, five boots gave 12-22. Without APST, seven gave 2-20
    (median 7).
  - Linux under `nomodeset` enters PS4 100 ms after I/O and shows 4.
  - So the trigger is hardware the platform runs as ToyOS leaves it. The one
    link setting that differs from Linux's is LTR_L1.2_THRESHOLD: 107,520 ns,
    as firmware left it, against Linux's 101,376 ns. The LTR max, T_PwrOn and
    T_CommonMode match.

**Exit**: the T14's `energy.idle.package` row is unresolved against Linux's
in `osbench_energy`, or the remaining gap is a cause named and recorded at its
owner.
