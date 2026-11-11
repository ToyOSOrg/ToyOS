---
status: open
kind: defect
opened: 2026-10-28
---

# Spawning a process costs ToyOS up to 147 times Linux's

`osbench`'s spawn rows spawn a program, wait for it, and time the whole.
`proc.spawn_wait.*` uses std's `Command`, `proc.posix_spawn_wait.*` C's
`posix_spawn` and `waitpid`, and `process.spawn_wait` spawns osbench itself.
`tiny` is a minimal program written to `/tmp`, `large` the same with 32 MiB
of data it never reads, and `std` the osbench binary. `thread.spawn_join.*`
creates and joins an empty thread. `work.build_shaped.j<cpus>` compresses 256
files, one child process each, as many at once as there are CPUs.

The current figures are the last table. Before `/tmp` was a file server's,
from the 2026-10-28 scoreboard (evidence branch `scoreboard-2026-10-28`):
ToyOS at 069cbbeb8, four boots off the internal NVMe, against five runs of
each bench's Linux as installed, interleaved. A cell is the p50 with its
run-to-run range. The AMD's Linux runs 1 to 3 timed through the TSC and 4
and 5 through the HPET, at 1.2 µs a timestamp.

| row | T14 ToyOS | T14 Linux | AMD ToyOS | AMD Linux |
|---|---|---|---|---|
| `proc.spawn_wait.tiny` | 52,548 ns (19.8%) | 52,094 ns (1.2%) | 69,724 ns (11.8%) | 266,663 ns (64.7%) |
| `proc.spawn_wait.large` | 202,300 ns (13.0%) | 54,906 ns (3.0%) | 170,284 ns (18.2%) | 273,444 ns (10.2%) |
| `proc.posix_spawn_wait.tiny` | 55,722 ns (10.1%) | 43,698 ns (2.4%) | 75,015 ns (4.2%) | 92,704 ns (150.0%) |
| `proc.posix_spawn_wait.std` | 451,798 ns (1.7%) | 169,630 ns (2.1%) | 504,838 ns (6.8%) | 327,668 ns (158.1%) |
| `process.spawn_wait` | 452,918 ns (4.7%) | 179,133 ns (6.0%) | 534,832 ns (3.8%) | 911,540 ns (64.2%) |
| `thread.spawn_join` | 28,144 ns (1.0%) | 25,040 ns (2.6%) | 41,094 ns (4.4%) | 72,607 ns (34.6%) |
| `thread.spawn_join.stack64k` | 28,562 ns (0.9%) | 23,441 ns (1.1%) | 40,322 ns (3.3%) | 65,844 ns (22.2%) |

| row | ToyOS | Linux |
|---|---|---|
| T14 `work.build_shaped.j8` | 166,245,528 ns (2.4%) | 99,148,404 ns (1.2%) |
| AMD `work.build_shaped.j16` | 270,675,582 ns (1.0%) | 116,739,823 ns (13.4%) |

**On the T14, then,** ToyOS lost every spawn and thread row but one. The
like-for-like pair is std's `Command` with the two programs: `tiny` reads
1.01x, unresolved, and `large` 3.68x, resolved. Under `Command`, then, what
costs ToyOS on that bench is what `large` adds, its 32 MiB in `/tmp`
(8,192 pages). `posix_spawn` of the osbench binary reads 2.66x and
`process.spawn_wait` 2.53x.

**On the AMD, then,** ToyOS resolved better on `spawn_wait.large`,
`posix_spawn_wait.tiny`, `spawn_wait.tiny` and every thread row, and stays
better against Linux's TSC runs alone (`large`: Linux 259 to 273 µs, ToyOS
155 to 186 µs). It reads better, unresolved, on `process.spawn_wait`, and
1.54x worse, unresolved, on `posix_spawn_wait.std`.
Linux's spreads there are its own: bimodal within the TSC runs
(`posix_spawn_wait.std` run 2 reads 740 µs), and a 1.2 µs timestamp is under
1% of a spawn.

**The all-CPU build lost on both benches**, 1.68x on the T14 (`j8`) and
2.32x on the AMD (`j16`), where ToyOS's one-at-a-time build (`j1`) beats
Linux's on both.

What `large` exercised then, a snapshot of the `/tmp` file that took a
reference to each resident page, is gone with the kernel's `/tmp`. The
spawner now reads a served image whole, through the pager, into fresh memory
(`issues/a-large-spawn-on-the-amd-bench-is-a-seventh-slower-than-mains.md`),
and shared text is stage 5 of
`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`.
`issues/idle-cpus-enter-the-c-states-cst-names.md` records the T14's `j1`
under deep idle.

**Current state, 2026-11-11** ([evidence](https://github.com/ToyOSOrg/evidence/tree/d44ebdae1f9f584d7b826e1498dbfd791a8d2339)): ToyOS at main ad5db5a65, four
boots a bench interleaved with five runs of its Linux as installed; a cell is
the median of the runs' p50s with their range, and the last column of each
bench is ToyOS against Linux, above 1 slower. The AMD bench's Linux timed
runs 1 to 3 through the HPET, so its medians overstate Linux's per-operation
times there.

| row | unit | T14 ToyOS | T14 Linux | T14 | AMD ToyOS | AMD Linux | AMD |
|---|---|---|---|---|---|---|---|
| `proc.posix_spawn_wait.std` | ns | 387,182 (0.7%) | 178,366 (3.3%) | 2.17x | 399,298 (27.4%) | 940,595 (60.3%) | 0.42x |
| `proc.posix_spawn_wait.tiny` | ns | 78,475 (5.0%) | 43,904 (3.0%) | 1.79x | 80,996 (14.5%) | 223,754 (60.1%) | 0.36x |
| `proc.spawn_wait.large` | ns | 8,072,387 (1.0%) | 54,731 (1.1%) | 147.49x | 12,586,708 (1.1%) | 278,643 (17.1%) | 45.17x |
| `proc.spawn_wait.tiny` | ns | 72,164 (7.3%) | 52,413 (2.1%) | 1.38x | 95,038 (22.1%) | 270,089 (9.9%) | 0.35x |
| `proc.spawn_wait_loop.100` | ns | 7,687,359 (10.5%) | 5,439,550 (2.7%) | 1.41x | 9,660,886 (15.6%) | 30,219,148 (20.5%) | 0.32x |
| `process.spawn_wait` | ns | 376,368 (1.3%) | 187,675 (2.2%) | 2.01x | 391,340 (4.2%) | 962,158 (63.8%) | 0.41x |
| `thread.spawn_join` | ns | 22,800 (9.7%) | 25,183 (3.2%) | 0.91x | 17,904 (1.7%) | 69,169 (31.9%) | 0.26x |
| `thread.spawn_join.default` | ns | 20,519 (11.2%) | 25,145 (0.5%) | 0.82x | 18,149 (0.5%) | 78,572 (11.4%) | 0.23x |
| `thread.spawn_join.stack64k` | ns | 21,230 (9.5%) | 23,558 (0.8%) | 0.90x | 15,514 (0.6%) | 73,335 (30.2%) | 0.21x |
| `work.build_shaped.j1` | ns | 690,704,458 (1.4%) | 1,013,128,216 (7.5%) | 0.68x | 886,883,802 (1.9%) | 1,639,173,487 (38.9%) | 0.54x |
| `work.build_shaped.j8` (T14), `.j16` (AMD) | ns | 220,886,682 (2.5%) | 99,510,091 (1.7%) | 2.22x | 277,092,641 (0.7%) | 107,734,386 (24.4%) | 2.57x |

`proc.spawn_wait.large` now takes 8.1 ms on the T14 and 12.6 ms on the AMD
bench in every boot, 147 and 45 times Linux's: the 32 MiB image is a served
file now, read through the pager
(`issues/a-large-spawn-on-the-amd-bench-is-a-seventh-slower-than-mains.md`).
On the T14 every process row is behind, 1.38x to 2.17x beside it, and the
thread rows are now ahead. On the AMD bench no other spawn or thread row is
behind: `posix_spawn_wait.std` and `process.spawn_wait` are unresolved against
Linux's 60% spread, and the rest are ahead. The all-CPU build is behind on both, 2.22x and 2.57x, where the
one-at-a-time build is ahead on both.

**Where the all-CPU build's time goes, 2026-11-11** ([evidence](https://github.com/ToyOSOrg/evidence/tree/a22307591cfdefcf9fa1fef2ae5d1de79b926e9e/spawnperf-diag)): one
diagnosis boot a bench at main 34cb4c918 with measurement commits, the build's
256 children at `j1` and at one a CPU, three rounds each, a child's CPU split by
system call, fault, lock spin and shootdown wait. The probes slow the round
(the AMD bench's `j16` round took 331 ms against 313 to 316 ms without them),
so the shares are of a slightly slowed machine. A child does the same work at
every width (53 system calls, 90 faults), and its CPU grows from 2.28 to 5.14 ms
on the T14 (`j8`) and from 2.83 to 15.5 ms on the AMD bench (`j16`). Of that
growth over the 256 children:

| | T14 `j1` -> `j8` | AMD `j1` -> `j16` |
|---|---|---|
| children's CPU, ms a round | 583 -> 1,316 | 724 -> 3,977 |
| `SHM_CREATE`, of the growth | 432 ms, 59 % (201 ms spin) | 2,342 ms, 72 % (1,934 ms spin) |
| faults, of the growth | 91 ms, 12 % (33 ms spin) | 456 ms, 14 % (324 ms spin) |
| every other system call | 28 ms | 57 ms |
| user time and exit, the rest | 182 ms | 400 ms |
| lock spin across the machine, a CPU's worth | 1.06 of 8 | 7.1 of 16 |
| of the contended lock waits, the frame allocator's one lock | 99 % | 99.7 % |
| shootdown waits, issued or served | none | none |

The four `SHM_CREATE`s a child makes are, by inference from the SDK, the
2 MiB windows of its four directory connections, each 512 frames drawn one at a
time under the frame allocator's one lock and zeroed: 95 µs a window on the T14
and 133 µs on the AMD bench even at `j1`, where a child's four are 17 and 19 % of
its CPU. At `j<cpus>` the draws queue on that lock. Lock spin across the machine
grows by 244 ms on the T14, a third of the children's growth, and by 2,354 ms
on the AMD bench, 72 %; the rest of the growth, user time and exit among it,
is not lock spin. The windows are
`issues/a-served-directory-costs-each-process-a-two-mebibyte-window.md`, the
lock `issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`.
The next contended lock, the process table's in `process_data`, waits 1.2 ms a
round on the T14 and 3.7 ms on the AMD bench; the kernel heap's 0.7 and 2.7 ms.
The rest's growth is not split further: the lock spin outside the children's
system calls and faults, which bounds the exit path's, is 10 and 77 ms.

Decided, in this order (the orchestrator, 2026-11-11): a draw of many frames
takes the allocator's lock once, with per-CPU frame caches held until that is
measured (the frame allocator's record); a shared-memory region's frames are
drawn at its pages' first touch, promised when it is made, and a device's run
stays drawn whole
(`issues/a-shared-memory-region-draws-and-zeroes-every-frame-when-it-is-made.md`);
and the window's size waits for the file-call work (the window's record). The
rest's growth is read again after the first two.

**Exit**: these rows within Linux's on both benches in a labelled
current-state comparison, or each remaining difference measured to its parts
and justified.

**Owner**: process creation (`kernel/src/process.rs`, the SDK's spawn and
`sdk/libc/src/spawn.rs`) and, for the parallel build, the frame allocator
(`kernel/src/mm/pmm.rs`), the shared-memory object (`kernel/src/object/shm.rs`)
and the file-call window (`sdk/toyos/src/fs.rs`).
