---
status: assigned
kind: defect
opened: 2026-11-11
---

# A shared-memory region draws and zeroes every frame when it is made

`SharedMemObject::create` (`kernel/src/object/shm.rs`) draws every frame of a
region and zeroes it before the call returns, so a region costs its whole size
in time and memory however little of it is touched, where a process's private
memory takes a frame at each page's first touch. On the spawn path it is a
build child's `SHM_CREATE`: 95 µs a call on the T14 and 133 µs on the AMD bench
with one child at a time, a child's four 17 and 19 % of its CPU, and most of
what the all-CPU build's children add in CPU; by inference from the SDK, those
four are the 2 MiB windows of its directory connections
(`issues/a-served-directory-costs-each-process-a-two-mebibyte-window.md`,
[evidence](https://github.com/ToyOSOrg/evidence/tree/a22307591cfdefcf9fa1fef2ae5d1de79b926e9e/spawnperf-diag)).

**Decided** (the orchestrator, 2026-11-11), second of three after the batched
draw (`issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`):
a region's frames are drawn at its pages' first touch. The whole region is
promised when it is made, so it is refused then and never at a touch. Two
kinds stay drawn whole when they are made: a device's run, as a device needs,
and an inbox's rings, which the kernel writes in interrupt context, where no
frame may be drawn. One draw serves both kernel-made zeroed frames for a
touched hole, `pmm::alloc_promised`: a region's page, drawn by
`SharedMemObject::page()` into the region's own slot for that page, and a
touched hole of a MEMORY volume's paged object (mapped paged objects, 7b in
`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`),
drawn into the pager's index under its lock. A region's slot is never revoked
and a pager's page is, so the two keep their own stores. The module header's
"a fault on a mapping of one never allocates" changes with it.

**Assigned**: the spawn-path work, after the batched draw lands.

**Exit**: a region's creation draws no frame and a first touch draws one,
promised whole at creation and refused only there, but for a device's run and
an inbox's rings, which are drawn whole when made; and a build child's
`SHM_CREATE` cost read again.
