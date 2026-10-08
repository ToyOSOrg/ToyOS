---
status: open
kind: tooling
opened: 2026-10-03
---

# `lan_hold` holds a boot open for a flat twenty seconds

`tests/toyos-rust-tests/src/bin/lan_hold.rs` sleeps `toyos_tco::LEASE_BOUND_MS`
and exits. The T14 boot `testcases-deaf` runs it as its one job, so the boot
does not end before the `dump-deaf-cpu` actuator has armed and dumped. It is a
fixed delay standing in for an event the job does not wait on, which root
`CLAUDE.md` forbids in a test.

## Exit condition

The boot's one job holds no sleep: it waits on the event it is held open for,
bounded by a timeout that panics by name, and `dump_nmi_probe` passes on a T14
run of that head.
