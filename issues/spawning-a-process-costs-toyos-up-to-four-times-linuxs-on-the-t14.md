---
status: open
kind: defect
opened: 2026-10-28
---

# Spawning a process costs ToyOS up to four times Linux's on the T14

`osbench`'s spawn rows spawn a program, wait for it, and time the whole.
`proc.spawn_wait.*` uses std's `Command`, `proc.posix_spawn_wait.*` C's
`posix_spawn` and `waitpid`, and `process.spawn_wait` spawns osbench itself.
`tiny` is a minimal program written to `/tmp`, `large` the same with 32 MiB
of data it never reads, and `std` the osbench binary. `thread.spawn_join.*`
creates and joins an empty thread. `work.build_shaped.j<cpus>` compresses 256
files, one child process each, as many at once as there are CPUs.

From the 2026-10-28 scoreboard (evidence branch `scoreboard-2026-10-28`):
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

**On the T14** ToyOS loses every spawn and thread row but one. The
like-for-like pair is std's `Command` with the two programs: `tiny` reads
1.01x, unresolved, and `large` 3.68x, resolved. Under `Command`, then, what
costs ToyOS on that bench is what `large` adds, its 32 MiB in `/tmp`
(8,192 pages). `posix_spawn` of the osbench binary reads 2.66x and
`process.spawn_wait` 2.53x.

**On the AMD** ToyOS resolves better on `spawn_wait.large`,
`posix_spawn_wait.tiny`, `spawn_wait.tiny` and every thread row, and stays
better against Linux's TSC runs alone (`large`: Linux 259 to 273 µs, ToyOS
155 to 186 µs). It reads better, unresolved, on `process.spawn_wait`, and
1.54x worse, unresolved, on `posix_spawn_wait.std`.
Linux's spreads there are its own: bimodal within the TSC runs
(`posix_spawn_wait.std` run 2 reads 740 µs), and a 1.2 µs timestamp is under
1% of a spawn.

**The all-CPU build loses on both benches**, 1.68x on the T14 (`j8`) and
2.32x on the AMD (`j16`), where ToyOS's one-at-a-time build (`j1`) beats
Linux's on both.

What `large` exercises is recorded. A spawn from a `/tmp` file snapshots it,
taking a reference to each resident page under the file cache's lock and 16
bytes a page (`issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md`,
"Spawn"), and shared text is stage 5 of
`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`.
This record adds the measured cost of that mechanism against Linux, the
osbench binary's spawn on the T14, the thread rows, and the parallel build,
whose loss where the serial build wins is unmeasured.
`issues/idle-cpus-enter-the-c-states-cst-names.md` records the T14's `j1`
under deep idle.

**Exit**: these rows within Linux's on both benches in a labelled
current-state comparison, or each remaining difference measured to its parts
and justified.

**Owner**: process creation (`kernel/src/process.rs`, `kernel/src/elf/`, the
SDK's spawn and `sdk/libc/src/spawn.rs`) and, for the parallel build, the
scheduler (`kernel/src/sched/`).
