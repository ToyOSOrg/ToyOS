---
status: open
kind: defect
opened: 2026-10-05
---

# The irq census summary takes a CPU's last stamped line as its newest read

`common::irqcensus::observe` (`tests/common/irqcensus.rs`) keeps, per guest
and CPU, the last `irq: cpuN` line the console carried, as if that line were
the CPU's newest read. It is not. A process exit reads the counters before
`log::emit` stamps its line, two exits on two CPUs run side by side, and the
shards are merged by stamp, so the line stamped last can carry the older
read. The suite's summary then counts that CPU short by what it took between
the two reads. The judge `irq_census` (`tests/toyos.rs`) folds each CPU's
lines with `Census::raise`, the largest count per source, for this reason.

Owner: the harness, `observe` in `tests/common/irqcensus.rs`.

**Exit:** `observe` folds each guest's CPU lines with `Census::raise`, and
`SEEN`'s doc no longer says the last line is the whole boot.
