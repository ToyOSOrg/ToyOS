---
status: open
kind: tooling
opened: 2026-11-04
---

# Many osbench rows on the AMD bench are bimodal per boot, so a few boots per arm compare mode draws

On the AMD bench (two LLCs, CPUs 0–7 and 8–15), many osbench rows land in one of two modes per boot, base images
included. Which mode a boot draws is mostly where its threads or processes were placed relative to each other, and that
placement is chance: spawn placement's rotation, and wherever earlier rows left the bench's main thread.

Evidence: #1129's ab9 and ab10, 18 benchcase boots across six arms
([ab10 judgment](https://github.com/ToyOSOrg/evidence/blob/d1dbb3c0a9511862295dab4b7cdcd1d24f47365a/pr-1129/r9/judgment.md),
[ab9 judgment](https://github.com/ToyOSOrg/evidence/blob/6b481f7d84081c39c3d9ee8160ea1bda0d1bb687/pr-1129/r8/judgment.md)).

| rows | modes | cause |
|---|---|---|
| `pipe.throughput.*` | per round 2.0 / 5.0, 3.5 / 8.4, 4.8 / 10.0 GB/s (4096, 16384, 65536) | the round's writer thread in the other LLC from its reader: measured per round, no exception in 18 boots |
| `thread.spawn_join` | 20 / 27 µs | the joined thread in the other LLC: 20.4–20.5 µs in all three boots of a near-placing spawn (#1153), 27 µs in 2 of 3 base boots |
| `proc.spawn_wait.tiny`, `proc.posix_spawn_wait.tiny`, `proc.spawn_wait_loop.100` | e.g. 3.9 / 5.6 ms per hundred | the share of tiny children ending in the other LLC from the parent (1–76% per boot, read off the kernel log's spawn and exit lines: [per boot](https://github.com/ToyOSOrg/evidence/blob/dd851b0675c29b428a0e46b817863f08e71cd2e3/pr-1153/r2/ab10-proc/proc-spawn-llc.txt)), which tracks the row |
| `ipc.reqrep.*`, `fs.disk.*` and `fs.disk.direct.*` metadata rows, `fs.mem.pread_8`, `persist.fsync_clean` | about 0.8× in one LLC | the client and its server in one LLC or split |
| `wake.periodic.idle` | 6 / 35 µs | not established |
| `crunch.single.deflate` | 63.7 / 67.5 ms | not established |

**The consequence for a decision rule:**
- A rule that compares arms by whether 3 boots' p50 ranges overlap counts each arm's mode draws on these rows. An arm
  that changes no placement can "resolve" against its base, as ab10's norule arm did on `fs.disk.direct.seek` and
  `wake.periodic.idle`. An arm that does change placement moves the draw rather than the cost.
- ab9 and ab10 blocked #1129 and #1153 partly on such rows.

**Wanted:** each row says which mode it measured, so a rule compares like with like.
1. **Where a row's placement is a fact of the system, osbench reports it per sample or round:** each side's most-picked
   CPU and LLC, and a class such as `llc=same|split`, as the pipe diagnostic did. Rules then compare within a class.
   `ran=` already gives the per-row process view, and the diary gives the per-thread one.
2. **Where placement is not what a row means to measure, the row controls it:** for example, a pair pinned once in one LLC
   and once split as two rows, as `pingpong` places its sides. The draw then disappears from the number.
3. **For a bimodal row with no placement cause yet** (`wake.periodic.idle`, `crunch.single.deflate`), report the inputs that
   might decide it, such as the CPU, its sibling's state and the idle depth. Until then a rule treats the row as a mode
   count.

Next measurement-infrastructure item; not built.
