---
status: open
kind: defect
opened: 2026-10-28
---

# `handle_lifetime`'s census wait never ends on the T14

In the T14's shared-debug boot, `test_rs_handle_lifetime` reaches
`kill_releases_ring` and never leaves it. The ring holder is killed at once,
then the test sleeps in `census_wait::released_to` until the 62.1 s list
bound ends it. `test_rs_ring0_timer_in_syscall`, `test_rs_shm_release_reclaims`,
`test_rs_spawn_child_ends_first` and `test_rs_spawn_lands_claimed` come after
it and never run.

Evidence (T14 readbacks):
- 2026-10-28: every boot at 576bebf3a, 7ef0a48c8, e33b248a2 and the
  igpu-fault head does this, nine boots in all.
- The last pass on record is 1595bbbcd (2026-10-23). There the same wait
  returned within a millisecond of the kill.

A T14 boot of 1090's first commit (abded5216) names the kind. One
`SharedMem` stays live for the whole minute, and the count does not move:
`census_wait: after 100 paces, still grown since the first reading: SharedMem 88 -> 89`.
Between the first reading and the wait, the only thing that happens is the
parent spawning the ring holder (pid 24) from `/system/bin` and killing it.
The earlier spawns (pids 22 and 23) fall before the first reading, which is
why this arm, the first to compare across a spawn, is the one that waits.

The leading hypothesis is that a spawn of a program from the file server
leaves one memory object alive after its child has ended. It is unconfirmed
which side keeps it: the parent's spawn, the file server's image answer, or
the kernel's teardown. Candidates in the window between 1595bbbcd and
576bebf3a are `53f73a63c` (a ROOT program spawned from SYSTEM's object for
it) and the memory stage's spawn changes. The acpiserver EC traffic that
only the failing boots show moves no count: the census stays at 89 across
its SCIs.

Owner: the spawn and memory-object path (`sdk/std/sys/process.rs`'s image
read, the file server's image answer, `kernel/src/loader`), until the holder
of the object is named.

**Exit**: whoever keeps the `SharedMem` past its child is named and fixed.
A spawn and the end of its child then leave the census where it was, shown
on QEMU and the T14. Five consecutive T14 shared-debug boots then run all
five members.
