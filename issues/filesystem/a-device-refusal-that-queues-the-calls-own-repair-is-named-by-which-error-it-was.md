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
refusal, not a wait"). The kernel adapter's `name_pending`
(`kernel/src/fat32_adapter.rs`) uses `waits_on` to choose the log line: an
`Io` in that state logs as "`<op>` of `<name>` refused, a repair pending with
`N` step(s) queued", while a `BudgetExpired` reaching the volume in the same
state — a repair now queued by this call's own write — logs as the call's own
failure, "`<op>` of `<name>`: budget expired". Two device refusals that leave
the volume in the same state (a repair queued, to be re-driven before the
next mutating call) are named two different ways depending only on which of
the two errors the device answered.

## Owner

`toyos-fat32::RepairNotice::waits_on`, whose contract is what a caller reads
after a refusal; the adapter's log line follows from what it answers.

## Exit condition

Either the two device refusals are named the same way when they leave the
volume in the same state, or the difference is stated as intentional at
`waits_on`'s doc comment with the reason a call's own `BudgetExpired` is
never read as "waiting" the way an `Io` is.
