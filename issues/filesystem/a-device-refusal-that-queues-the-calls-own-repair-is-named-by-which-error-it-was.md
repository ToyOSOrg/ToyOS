---
status: open
kind: finding
opened: 2026-09-26
---

# A device refusal that queues the call's own repair is named by which error it was

`RepairNotice::waits_on` (`toyos-fat32/src/repair.rs`) answers `true` for
`Error::Io` and `Error::RepairPending`, and `false` for
`Error::BudgetExpired` — even when the `BudgetExpired` is the call's own write
and that same write is what queued the repair
(`toyos-fat32/tests/refused_writes.rs:729`, asserted directly: "its own
refusal, not a wait"). Two device refusals that leave the volume in the same
state (a repair queued, to be re-driven before the next mutating call) are
named two different ways depending only on which of the two errors the device
answered. No caller reads the difference today: the kernel adapter that chose
its log line by `waits_on` is gone, and fsd's disks refuse on no clock, so it
never meets a `BudgetExpired` — the next caller that does inherits it.

## Owner

`toyos-fat32::RepairNotice::waits_on`, whose contract is what a caller reads
after a refusal; the adapter's log line follows from what it answers.

## Exit condition

Either the two device refusals are named the same way when they leave the
volume in the same state, or the difference is stated as intentional at
`waits_on`'s doc comment with the reason a call's own `BudgetExpired` is
never read as "waiting" the way an `Io` is.
