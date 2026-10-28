---
status: open
kind: defect
opened: 2026-10-28
---

# Spawning a process costs ToyOS up to four times Linux's on the T14

`osbench`'s spawn rows spawn a program, wait for it, and time the whole:
`proc.spawn_wait.*` by std's `Command`, `proc.posix_spawn_wait.*` by C's
`posix_spawn` and `waitpid`, and `process.spawn_wait` this program itself.
`tiny` is a minimal program, `large` the same with 32 MiB of unread data, and
`std` the osbench binary. `thread.spawn_join.*` creates and joins an empty
thread. `work.build_shaped.j<cpus>` compresses 256 files, one child process
each, as many at once as there are CPUs. The 2026-10-28 scoreboard (ToyOS at 069cbbeb8, 4 boots off the internal NVMe against 5 runs of each bench's Linux as installed, interleaved; p50s, with the run-to-run range; evidence branch `scoreboard-2026-10-28`):

| row | T14 ToyOS | T14 Linux | AMD ToyOS | AMD Linux |
|---|---|---|---|---|
| `proc.spawn_wait.large` | 202,300 ns (13.0%) | 54,906 ns (3.0%) | 170,284 ns (18.2%) | 273,444 ns (10.2%) |
| `proc.posix_spawn_wait.std` | 451,798 ns (1.7%) | 169,630 ns (2.1%) | 504,838 ns (6.8%) | 327,668 ns (158.1%) |
| `process.spawn_wait` | 452,918 ns (4.7%) | 179,133 ns (6.0%) | 534,832 ns (3.8%) | 911,540 ns (64.2%) |
| `proc.posix_spawn_wait.tiny` | 55,722 ns (10.1%) | 43,698 ns (2.4%) | 75,015 ns (4.2%) | 92,704 ns (150.0%) |
| `thread.spawn_join.stack64k` | 28,562 ns (0.9%) | 23,441 ns (1.1%) | 40,322 ns (3.3%) | 65,844 ns (22.2%) |
| `thread.spawn_join` | 28,144 ns (1.0%) | 25,040 ns (2.6%) | 41,094 ns (4.4%) | 72,607 ns (34.6%) |
| `work.build_shaped.j<cpus>` | 166.2 ms (2.4%), j8 | 99.1 ms (1.2%), j8 | 270.7 ms (1.0%), j16 | 116.7 ms (13.4%), j16 |

On the T14 ToyOS loses every row here, 1.1x to 3.7x, each resolved. The cost
grows with the program's size: 3.7x for `large` and 2.5 to 2.7x for the osbench
binary, against 1.3x for `tiny`. The AMD's spawns and thread creations read
better for ToyOS, but two of five Linux runs there timed through the HPET.
The all-CPU build loses on both benches: 1.7x on the T14 and 2.3x on the AMD,
where ToyOS's one-at-a-time build (`j1`) is faster than Linux's on both. What a
spawn spends on the program's size, and why the parallel build loses where
the serial one wins, is unmeasured.
`issues/idle-cpus-enter-the-c-states-cst-names.md` records the T14's `j1`
under deep idle.

**Exit**: these rows within Linux's on both benches in a labelled
current-state comparison, or each remaining difference measured to its parts
and justified.

**Owner**: process creation (`kernel/src/process.rs`, `kernel/src/elf/`, the
SDK's spawn and `sdk/libc`'s `posix_spawn`) and, for the parallel build, the
scheduler (`kernel/src/sched/`).
