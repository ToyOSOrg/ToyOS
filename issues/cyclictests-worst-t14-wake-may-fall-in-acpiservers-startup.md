---
status: open
kind: finding
opened: 2026-10-22
---

# cyclictest's worst T14 wake may fall in acpiserver's startup

One T14 `latencycase` boot at `e6d6170d8` (#1009), the first with
`/system/bin/acpiserver` started, read `cyclictest: 10000 wakes at 200us:
p50=4us p90=6us p99=16us p99.9=74us max=1874us`. The worst wake is stamped
638,179,441 ns on `nanos_since_boot`, and cyclictest ran from 12.082 s to
14.103 s on the log's clock.

acpiserver's startup overlaps the run's first 90 ms:
- `ACPI_ENABLE` at 12.075 s;
- the namespace initialised at 12.160 s;
- power sources read at 12.171 s.

In that window the tables' bytes took 42,906 reads of firmware memory, and
their AML read SystemMemory 385 times. The boot logged firmware call `0xf5` at
12.151 s, which held cpu0 297,820 ns. The SMI count on every CPU moved from
4819 to 4821 across cyclictest's run. One step is that call. The other is not
attributed: the kernel logs only the first call of each byte.

The kernel's own lines put power-on to the kernel at 11.463 s, so the worst
wake would be near 12.10 s, inside the window. That is an inference: the two
clocks have not been aligned.

**Owner**: `issues/toyos-beats-linuxs-latency-on-the-t14.md`.

**Next step**: align cyclictest's worst-wake stamp with the log's clock. Then
read the kernel's firmware-call count and time at the stop, to attribute the
second SMI.

**Exit**: the worst wake's cause is named, either as acpiserver's startup or
as something else. If it is the startup, the owner decides whether timing rows
start after it.
