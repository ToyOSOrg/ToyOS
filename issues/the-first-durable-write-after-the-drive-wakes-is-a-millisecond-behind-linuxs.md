---
status: open
kind: defect
opened: 2026-11-11
---

# The first durable write after the drive wakes is a millisecond behind Linux's

With Autonomous Power State Transitions on, both systems set the same table
on both benches' drives (`0,1,2,3->4 after 100 ms`), and the first durable
write after 10 s of idle pays the drive's wake on both. ToyOS pays more.
`persist.write_4k_fsync_after_idle` on 2026-11-11, three boots of
`047e0a708` against three Linux runs, median of p50s (evidence `pr-1131`
r10):

| bench | ToyOS | Linux | remainder | worst sample, ToyOS against Linux |
|---|---|---|---|---|
| T14 | 14.31 ms (14.20-14.51) | 13.02 ms (12.99-13.04) | 1.29 ms, 10% | 18.34 against 13.75 ms |
| AMD | 11.33 ms (11.15-11.34) | 10.70 ms (10.58-11.47) | 0.63 ms, 6% | 11.78 against 11.83 ms |

On the AMD bench ToyOS's boots lie inside the range of Linux's runs. On the
T14 they do not, and two of its three boots had a worst sample at 18 ms.

The wake itself Linux pays too, and this record is the remainder, not the
wake. On the AMD bench the row reads 4.19 ms without APST, so the wake is
about 7.1 ms. On the T14 the row without APST measures no wake: every boot
without APST ran in the drive's slow write mode
(`issues/toyos-durable-writes-on-the-t14s-nvme-run-six-times-slower-in-some-boots.md`).
Within the fast mode, the boot with APST that ran fast throughout read
14.20 ms after idle against 1.10 ms back to back, a wake of about 13.1 ms;
Linux read 13.02 against 0.98 ms, about 12.0 ms.

**What is known of the cause.** Before #1152 the T14's sample was split
command by command (evidence `pr-1131` r7): the drive's wake, the first
write and first Flush, matched Linux's whole extra, and ToyOS's own share was
the commit's second Flush, 2.25 to 2.5 ms after a wake against 0.4 ms warm,
with about 1 ms of host time between commands. The journal's commit is
still a flush, the entry and a flush (`write_entries`,
`lib/bcachefs/src/engine/mount/mod.rs`), so the second Flush is the first
suspect. It has not been split again on this path. Replacing that Flush by
a forced write was measured and declined for its cost on every other
durable write
(`issues/a-forced-journal-write-is-no-faster-than-a-write-and-a-flush.md`).

**Exit**: on the T14, three boots' `persist.write_4k_fsync_after_idle` p50
within the range of Linux's runs in the same session, with no sample past
Linux's worst; or the remainder split per command on the current commit
path and attributed, with what it would cost to remove.

**Owner**: the DATA volume's commit (`lib/bcachefs/src/engine/mount/`) and
the NVMe driver (`system/nvme/`).
