---
status: open
kind: defect
opened: 2026-10-02
---

# A kill on a spawn's handle before its child lands claims nothing

`PendingHandles::commit` (`kernel/src/loader/start.rs`) puts the spawner's
handle to its child in the spawner's table before `loader::spawn` lands the
child in the process table, and a handle's number is its table's own
arithmetic, so another thread of the spawner can name the handle in between.
A `SYS_PROCESS_KILL` on it there claims nothing:
`toyos_proclife::teardown::claim_teardown` answers `false` for a pid not in
the table, and `process::kill_process` answers `Ok`. The child then lands and
runs. `kill_process`'s "`Ok` for an already-gone process: the caller asked for
it to be dead and it is" does not cover a process not yet there.

Read off the code; no test reaches it, and no caller can order a kill inside
a spawn. A close, a dup, a transfer, a wait and a stats read of the handle in
the same window are sound.

**Owner**: the spawn's commit, `kernel/src/loader/start.rs`.

*Exit*: a kill on a handle whose process has not landed ends that process —
the landing claims it, as it claims a child under a claimed place — or the
handle resolves only once the child has landed; `toyos-proclife` scripts the
kill as a step between the commit and the landing, and no schedule leaves the
child running.
