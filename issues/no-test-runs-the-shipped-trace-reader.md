---
status: open
kind: defect
opened: 2026-10-04
---

# No test runs the shipped `/system/bin/trace`

`userland/trace` ships on the `trace` right `system.toml` grants it, and no
test runs it: not its lines, and none of its refusals, each exit 2 with a
reason on stderr — no system capability, a capability without `trace`, a
record that does not decode. `test_rs_trace_read` reaches the syscall and the
decoder underneath it, not the program. A test of the program needs a boot
whose image carries it, which `tests/virtsmpcase` does not (its jobs are the
suite binaries the harness puts on ROOT).

Owned by step 2 of
`issues/the-diary-computes-no-lateness-and-records-no-slow-system-call.md`,
whose lateness reader reads the same rings. **Exit**: a test runs the program
with its right and reads its lines back, and runs it with no capability and
with one lacking `trace`, and reads exit 2 and the reason for each.
