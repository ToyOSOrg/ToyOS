---
status: assigned
kind: defect
opened: 2026-10-12
---

# The kernel brings up at most eight CPUs

Every per-CPU structure in the kernel is a `static` array of `MAX_CPUS = 8`,
declared three times (`kernel/src/sched/mod.rs`, `kernel/src/smp_roster.rs`,
`kernel/src/shootdown.rs`) and pinned equal by `const` assertions. The roster
refuses an id at or above it, so a machine whose firmware names more CPUs runs
on the first eight in MADT order and leaves the rest off. The ABI carries the
same number twice: `MAX_LOG_SHARDS = 8` is `LogCursor::next`'s width, which the
trace diary's cursor shares, and `TraceRecord::cpu` is a `u16`.

**Evidence.** The AMD bench's kernel log posted on PR #852
(https://github.com/ToyOSOrg/ToyOS/pull/852#issuecomment-6107031099):
`ACPI: MADT cpus=[0, 1, …, 15]`, then
`SMP: roster full at 8 CPUs; ignoring further MADT entries`, then
`SMP: 8 of 16 MADT cpus online`. Half its CPUs run nothing.

**Owner:** the orchestrator, which holds the design round in PR #865.

**Exit:** `git grep -n 'MAX_CPUS\|MAX_LOG_SHARDS' -- kernel sdk` finds
nothing, and the AMD bench's `smp_roster_and_tsc_trail` row logs
`SMP: 16 of 16 MADT cpus online`.
