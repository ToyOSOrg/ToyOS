---
status: open
kind: defect
opened: 2026-10-04
---

# The `irq_census` judge reds on two exits' lines stamped in the other order from their reads

The judge `irq_census` (`tests/toyos.rs`) compares lines from different process
exits as if the capture listed them in the order their counters were read. It
does not. Each exit prints `irq_census::log_census` and then
`tlb::log_census` (`kernel/src/process.rs`), and nothing serialises two exits on
two CPUs. `log::emit` reads its arguments and formats them before it stamps the
record (`kernel/src/log/mod.rs`), each CPU writes its own shard, and
`log::read::drain_ordered` merges the shards by stamp. So exit X can read a
counter before exit Y and still be stamped after Y.

Two of the judge's checks then red with no kernel defect:

- **The per-source monotonic check.** X reads cpu1's census, then Y reads it
  with one more `kick`. Y is stamped first. The capture shows cpu1's `kick`
  going backwards.
- **The issuer checks on `tlb: shootdowns=`.** Y loads `ISSUED` and gets T1.
  X then loads T2 > T1, swaps `REPORTED` to T2 and logs. Y's swap returns T2,
  which is not T1, so Y logs `shootdowns=T1` after X's T2. The capture shows
  `T2` then `T1`, which reds "the issuer census went backwards". The last
  `tlb:` line is then T1, and a newer `irq:` line's `tlb` count can exceed it,
  which reds "some path shoots down without being counted".

Neither has been seen red. The recorded red the same window produced was the
deleted total/sources check (#734), where the reads were within one line.

Owner: the harness, `irq_census` in `tests/toyos.rs`.

**Exit:** a host test feeds the judge two exits' `irq:` and `tlb:` lines in
stamp order, with the later stamp carrying the earlier read as above, and the
judge stays green.
