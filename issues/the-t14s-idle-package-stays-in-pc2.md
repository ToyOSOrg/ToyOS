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

APST is the larger share and comes first.

**Exit**: the T14's `energy.idle.package` row is unresolved against Linux's
in `osbench_energy`, or the remaining gap is a cause named and recorded at its
owner.
