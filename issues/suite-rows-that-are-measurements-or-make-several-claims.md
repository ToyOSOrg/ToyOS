---
status: open
kind: defect
opened: 2026-10-10
---

# Suite rows that are measurements or make several claims

`reviewer.md`'s **Tests**: a test makes one claim, under a name that says it, and a measurement,
a number held to no bound, never becomes a suite row. The owner ruled the one-claim reading a
BLOCKER for a new or changed test; these stand in the tree today.

Measurements as `METAL` rows in `tests/toyos.rs`:

- `syscall_cost` and `spawn_cost`: each verdict is that its numbers were printed.
- `trace_record_cost`: the cost is "read, not held", while `trace_flood.rs` also asserts the
  ring's accounting, which the row's name does not say.

`METAL` rows that make several claims:

- `counters` (`counters_on_metal`): records fresh and named by APIC id, SMI counts moved only by
  the kernel's commands, MPERF against the stamp, an idle second with no log line, the HWP request
  and a turbostat frequency floor. The HWP request's oracle is
  `issues/t14-rows-hold-toyos-to-linuxs-choice-or-copy.md`'s.
- `acpi_tables_loaded` (`acpi_tables_on_metal`): the definition-block count, the MTRRs compared
  across CPUs, nothing refused, and the AML reads. The count's oracle is the same issue's.
- `timer_calibration`: both clocks calibrated, and the TSC within 1% of CPUID.

Guest binaries in `tests/toyos-rust-tests/src/bin/` that make several claims:

- `allocator_stress.rs`: twelve checks.
- `std_process.rs`: eight std process behaviours.
- `std_sync.rs`: mutex, condvar, park and rwlock.
- `sched_stress.rs`: io_uring acceptor isolation, the post-wake vruntime lag, and a connect storm.
- `toybox_file_tools.rs`: `cp`, `mv` and `hexdump`.
- `std_fs.rs`: listing, a self-read, NotFound, and file types.

Owner: the orchestrator, which dispatches the reshape.

**Exit condition**: `syscall_cost` and `spawn_cost` are each held to the T14's record or deleted
with their binaries; `trace_record_cost` is named for the ring-accounting claim and its cost
either held to the record or not printed; and every row and binary above makes one claim under a
name that says it.
