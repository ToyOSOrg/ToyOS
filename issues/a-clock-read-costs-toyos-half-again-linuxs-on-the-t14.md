---
status: open
kind: defect
opened: 2026-11-11
---

# A clock read costs ToyOS half again Linux's on the T14

`osbench time`'s `time.now` times std's `Instant::now`
(`tests/guest/osbench/src/time.rs`).

From the 2026-11-11 scoreboard ([evidence](https://github.com/ToyOSOrg/evidence/tree/d44ebdae1f9f584d7b826e1498dbfd791a8d2339)): ToyOS at main ad5db5a65,
four boots a bench off its NVMe, interleaved with five runs of the bench's
Linux as installed. A cell is the median of the runs' p50s with their
run-to-run range. The AMD bench's Linux timed runs 1 to 3 through the HPET
and runs 4 and 5 through the TSC.

| row | T14 ToyOS | T14 Linux | AMD ToyOS | AMD Linux, TSC runs |
|---|---|---|---|---|
| `time.now` | 21,005 ps (0.0%) | 14,283 ps (0.1%) | 9,388 ps (0.8%) | 22,599 and 22,692 ps |

On the T14 every boot read 21.0 ns against Linux's 14.3 ns in every run,
1.47 times. On the AMD bench ToyOS reads 9.4 ns, under half of Linux's TSC
runs; its HPET runs read 1.2 µs. The 2026-10-28 scoreboard read the T14 at
19.8 against 14.3 ns. What the T14's read spends its 7 ns on is unmeasured.

#1240 reads the counter with `RDTSCP` and nothing after it, where the read
was `LFENCE; RDTSC; LFENCE`, and keeps the page's nanoseconds in std's
`Instant`, which divided every reading by 10^9. Its speed-up is not yet
measured on metal.

- **Owed on metal:** an A/B of main before and after #1240, on the T14 and
  the AMD bench: `osbench time`'s `time.now`, four boots a side in rotated
  order, each boot's p50, compared by an exact two-sided Mann-Whitney test,
  with the rest of `osbench time`, `files`, `ipc` and `sched` as controls;
  and Linux's TSC-timed `osbench time` on each bench. Main after must read
  `time.now` lower than main before in every T14 boot, set against Linux's
  14.3 ns there, with no control row 5% or more worse unless the code #1240
  changed explains it. On the AMD bench a rise is recorded as undetermined
  between ordering's price and a regression, unless a ToyOS boot logs MSR
  C001_1029 bit 1 before writing it (#1243's boot line): on that bench's
  family the bit decides whether main before's read was ordered at all.

**Exit**: `time.now` within Linux's on the T14 in a labelled current-state
comparison, or the difference measured to its parts and justified.

**Owner**: the clock read (std's `Instant` on ToyOS and the SDK's clock).
