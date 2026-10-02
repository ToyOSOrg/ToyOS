---
status: open
kind: tooling
opened: 2026-10-02
---

# A metal judge takes a name's lowest pid for the job

`Readback::exit_code` (`tests/common/metal.rs`) reads a job's exit off the
record with the lowest pid among those of its name: a binary that re-executes
itself leaves one record per child under that name, and the runner spawned the
job before the job spawned anything.

The kernel does not issue pids in landing order. A spawn refused after its
admission gives its pid back, and the next admission takes it
(`toyos_proclife::pids`). A spawn admitted before a job and refused after the
job landed hands a pid below the job's to the next spawn, and if that is the
job's child of the same name the judge reads the child's exit as the job's.

Evidence: by reading. No boot has shown it: the runner starts one job at a
time, so it takes another process inside `SYS_SPAWN` across a job's start.

Exit: the judge finds the job's record by something only the job has (the pid
the runner records for it), and a host test over a log in which a child holds
the lower pid reds the lowest-pid rule. Owner: the harness.
