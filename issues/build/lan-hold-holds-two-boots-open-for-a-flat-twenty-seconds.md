---
status: open
kind: finding
opened: 2026-10-03
---

# `lan_hold` holds two boots open for a flat twenty seconds

`tests/toyos-rust-tests/src/bin/lan_hold.rs` sleeps `toyos_tco::LEASE_BOUND_MS`
and exits. Two T14 boots run it as their one job: `lanleasecase`, so the boot
does not end before netd's `--exit-with-lease` has, and `testcases-deaf`, so
it does not end before the `dump-deaf-cpu` actuator has armed and dumped. Each
is a fixed delay standing in for an event the job does not wait on, which root
`CLAUDE.md` forbids in a test.

On the T14 run of #638's head netd exited at 19.212 s of the `lanleasecase`
boot and init stopped the machine at 21.202 s.
