---
status: assigned
kind: defect
opened: 2026-10-31
---

# The AMD bench's idle package draws 15 % over Linux's

Held by the power task that follows #1100.

On the AMD bench (Family 17h Model 60h), `osbench_energy` resolves the idle
package against ToyOS. Over four runs a side, every ToyOS run lies above
every Linux run (p = 0.029):

- The median of the runs' per-second p50s is 1688 mW against 1472 mW.
- The median of the runs' means is 1688 mW against 1501 mW.

The cores are not the consumer. Their own energy counters give ToyOS 0 mW
over the idle minute and Linux 11 mW, so the gap of about 200 mW lies
outside the cores: in the SoC, memory, I/O or devices. Its 10 ms sleeper,
1700-1710 against 1391-1705 mW, is not resolved. Work may not be slowed to
close the gap.

The bench documents no package C-state residency (PPR 55922), so ToyOS's
rows cannot show which state the package reaches.

**Exit**: the AMD bench's `energy.idle.package` row is unresolved against
Linux's in `osbench_energy`, or the remaining gap is a cause named and
recorded at its owner.
