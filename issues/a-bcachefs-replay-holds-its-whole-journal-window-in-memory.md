---
status: open
kind: defect
opened: 2026-11-04
---

# A bcachefs replay holds its whole journal window in memory

**Owner.** The storage track, which owns `lib/bcachefs`.

An unclean mount puts every key of the journal window into pending
(`journal::replay`, `lib/bcachefs/src/engine/pending.rs`), each accounting
delta its own entry. mkfs sizes the journal at 1/128 of the device, and
reclaim trims it only under journal pressure, so on a large DATA the window
holds whatever ran since the last pressure.

**Evidence.** A host model of the T14's DATA, written with osbench's
persist and `fs.disk` call sequence and dropped unclean, replays about
1.5 million keys, most of them accounting deltas at a handful of positions.
The file server's process peaks at 1.0 GB resident (`/usr/bin/time -l`)
and the mount takes 3 to 3.6 s, before the first transaction takes the
deltas into their leaf in 0.3 to 0.4 s (#1152, the osbench_persist hang
investigation; its harness and logs are in the evidence).

**Exit.** Replay memory bounded by something smaller than the journal, for
instance accounting deltas summed per position as they replay, against the
version their leaf holds, and a journal window trimmed by its size as well
as by pressure. A host test that replays a window of many transactions and
bounds pending's bytes.
