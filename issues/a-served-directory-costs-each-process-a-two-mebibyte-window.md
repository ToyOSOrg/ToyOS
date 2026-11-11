---
status: open
kind: tooling
opened: 2026-09-27
---

# A served directory costs each process a 2 MiB window

Every process that names a file under a served directory connects to that
directory's server once and lends it a `toyos::fs::WINDOW_BYTES` region
(`sdk/toyos/src/fs.rs`): 2 MiB of the process's memory per directory it has
touched, held for the process's life, and mapped into the server too. A
desktop process that touches `/home`, `/config`, `/state` and `/apps` holds
8 MiB of windows; fsd maps up to `MAX_SERVED` of them. Whether the window's
size is paid for by the reads that fill it, and what the desktop's processes
hold in windows together, has not been measured. The size's stated reason,
that 2 MiB is the size shared memory comes in, is gone: a region is whole
4 KiB pages.

**Creating it is most of what the all-CPU build's children add in CPU**
(2026-11-11, [evidence](https://github.com/ToyOSOrg/evidence/tree/a22307591cfdefcf9fa1fef2ae5d1de79b926e9e/spawnperf-diag); the attribution is in
`issues/spawning-a-process-costs-toyos-up-to-147-times-linuxs.md`). A build
child makes four `SHM_CREATE`s beside its four directory connections, by
inference from the SDK their windows. Each is 512 frames drawn one at a time
under the frame allocator's one lock and zeroed: 95 µs a window on the T14 and
133 µs on the AMD bench with one child at a time, where a child's four are 17
and 19 % of its CPU. With one child a CPU, `SHM_CREATE` is 59 % (T14) and 72 %
(AMD) of the growth in the children's CPU; of that, 47 % (T14) and 83 % (AMD)
is spin on that lock
(`issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`),
and the T14's other 231 ms is not split. How much of the build's wall-clock
loss against Linux this is has not been measured; that it is most of it is an
inference.

**Decided** (the orchestrator, 2026-11-11), third of three: the window's size
waits for the file-call work. Once a served file's reads, writes and
attributes go through its paged object (PR #1258 and the work after it), the
window carries paths, small answers and the writes a holder's admission
refuses, and is cut to what that needs. Before that, its creation costs what
its region's first-touch draw makes it
(`issues/a-shared-memory-region-draws-and-zeroes-every-frame-when-it-is-made.md`).

- **Owed on metal:** the first-touch region (a window's frames drawn at its
  pages' first touch). An A/B of main before and after it on the T14 and the
  AMD bench, of `osbench work` (`work.build_shaped.j1`, and `.j8` on the T14
  and `.j16` on the AMD bench), `spawn_cost`, `osbench procs` and `osbench
  files`. Main after must show `work.build_shaped.j1` and `.j<cpus>` faster on
  both benches, and no `proc` or `fs` row slower beyond the before run's
  spread: `proc.spawn_wait.large`, whose 32 MiB image region is written whole
  and now draws a frame a page as it is written, most of all.

**Exit**: the resident memory of a desktop boot's processes with and without
their windows, measured, and the window's size justified from it or cut once
the file-call work carries reads and writes on the paged object.
