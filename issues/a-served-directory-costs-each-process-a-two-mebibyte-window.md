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

**Its creation is most of what the all-CPU build loses** (2026-11-11,
[evidence](https://github.com/ToyOSOrg/evidence/tree/36117dda5c9a1a3803745c45785cc8243c5b5d51/spawnperf-diag); the attribution is in
`issues/spawning-a-process-costs-toyos-up-to-147-times-linuxs.md`). A build
child makes four `SHM_CREATE`s beside its four directory connections, by
inference from the SDK their windows. Each is 512 frames drawn one at a time
under the frame allocator's one lock and zeroed: 95 µs on the T14 and 133 µs
on the AMD bench with one child at a time, 17 and 19 % of a child's CPU, and
with one child a CPU 59 % (T14) and 72 % (AMD) of the growth in the
children's CPU, most of it spun on that lock
(`issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`).

Proposed, not built, either of:

- **The window is as large as what it still carries.** Once a served file's
  reads and writes go through its paged object (PR #1258 and the work after
  it), the window holds paths, small answers and the writes a holder's
  admission refuses; it is cut to the largest of those, with a larger write
  split. 64 KiB would be 16 frames a connection.
- **A region's frames are drawn at first touch.** Promised whole when it is
  made, so it is still refused then and never at a touch, and each frame drawn
  and zeroed by the first fault or kernel copy that reaches its page, so a
  window costs what its requests touch. It changes every region the kernel
  lends but a device's run, which is laid out apart.

**Exit**: the resident memory of a desktop boot's processes with and without
their windows, measured, and the window's size justified from it or cut; and
a build child's window cost read again after the change.
