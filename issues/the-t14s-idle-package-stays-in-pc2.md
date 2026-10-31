---
status: assigned
kind: defect
opened: 2026-10-31
---

# The T14's idle package stays in PC2

Held by the power task that follows #1100.

On the T14, `osbench_energy` gives an idle package of 7137 mW under ToyOS
against 1648 mW under Linux, 4.3×. Under a 10 ms sleeper it is 2863 against
1219 mW. ToyOS's package spends 994 ‰ of the idle minute in PC2 and none
deeper, against Linux's 102 ‰ in PC2. Every ToyOS core waits in C7. Power
belongs in the idle states and in device power management: none of this gap
may be closed by slowing work. The AMD bench is at 1.15×.

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

Still to separate:

- **the display engine** scanning out under the firmware's GOP programming,
  with no DMC firmware. Intel's Tiger Lake display reference (Vol. 12, "Display
  C5 and C6") requires DMC for DC5 and DC6.
- **the NVMe** in its operational state, with no APST
  (`issues/diskserver-drives-nvme-apst.md`), and its link's power state.
- **the I219** under netstack, through its LTR.

**Exit**: the T14's `energy.idle.package` row is unresolved against Linux's
in `osbench_energy`, or the remaining gap is a cause named and recorded at its
owner.
