---
status: assigned
kind: defect
opened: 2026-11-11
---

# A shared-memory region draws and zeroes every frame when it is made

`SharedMemObject::create` (`kernel/src/object/shm.rs`) draws every frame of a
region and zeroes it before the call returns, so a region costs its whole size
in time and memory however little of it is touched, where a process's private
memory takes a frame at each page's first touch. A served directory's 2 MiB
window is the largest such region on the spawn path: 95 µs a window on the T14
and 133 µs on the AMD bench with one build child at a time, a child's four 17
and 19 % of its CPU, and most of what the all-CPU build's children add in CPU
(`issues/a-served-directory-costs-each-process-a-two-mebibyte-window.md`,
[evidence](https://github.com/ToyOSOrg/evidence/tree/36117dda5c9a1a3803745c45785cc8243c5b5d51/spawnperf-diag)).

**Decided** (the orchestrator, 2026-11-11), second of three after the batched
draw (`issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`):
a region's frames are drawn at its pages' first touch. The whole region is
promised when it is made, so it is refused then and never at a touch; a
device's run stays drawn whole, as a device needs. It is one mechanism with
the kernel's zeroed frame for a touched hole of a MEMORY volume's paged
object, which mapped paged objects bring (7b in
`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`), and
is built on 7b's object mappings (#1259). The module header's "a fault on a
mapping of one never allocates" changes with it.

**Assigned**: the spawn-path work, after the batched draw lands.

**Exit**: a region's creation draws no frame and a first touch draws one,
promised whole at creation and refused only there; and a build child's
`SHM_CREATE` cost read again.
