---
status: open
kind: tooling
opened: 2026-09-23
---

# `toyos-i219`'s MDIC interrogation is a bench instrument, and it is owed a deletion once the unready transaction is named

`toyos-i219/src/unready.rs` is not a driver. It runs on one boot, inside the
crumb arm, and only where `phy::bring_up` answered `PhyRefusal::MdiUnready` —
the wall the bring-up on the T14 hits, where §10.2.2.7's `Ready` never comes
back for the first MDI transaction and the durable trail carries one
`read MDIC` and then the deadline, so what the poll saw is unread. It takes
§4.5.2's interface again, asks the part three transactions with one variable
between them, and leaves every reading of `MDIC` on the stick as a
`Step::Saw` crumb.

It exists because the driver's own refusal carries a register it never reports:
`MdiUnready` says a transaction did not end and cannot say what stood in
`MDIC` while it did not, whether `Error` was set instead of `Ready`, or whether
the same transaction ends when it is polled a second apart instead of a
microsecond apart.

**What it costs.** Two durable lines per reading of `MDIC`, `SAMPLES` readings
per paced transaction, two paced transactions and — only where something ended
and the identifier did not answer at §9.3's first address — up to
`SWEEP_SAMPLES` more per PHY address over all thirty-two. Every one of those
lines is a stick write on the boot it is left on, and the window netd holds the
card for grows by all of them.

**What it is allowed to reach.** `MDIC` and `EXTCNF_CTRL`, and nothing else —
which is what lets `crumbs::is_the_phys` take every line it leaves out of a
trail before the harness holds that trail to `crumbs::BRING_UP`. A reading of
any other register would have to move the harness's model of the trail with it.

## Owner

The I219 bring-up's author, and after it the network track.

## What would close it

Why that first transaction never reports `Ready` on this part — named from a
trail this instrument leaves, or from a reading that makes the question moot
(a bring-up that gets past it). Then the module goes, with
`crate::I219::registers`, the `pub(crate)` on `phy::Owned`, `phy::Arbitration`,
`Owned::claim`, `Owned::transact` and `Owned::part`, `crumbs::Step::Saw` and
its two arms in `Step::parse` and `Named`, the call in
`userland/netd/src/i219.rs`'s `leave_crumbs`, and the four tests in
`toyos-i219/src/tests.rs` that name it. `phy::command` stays: `transact` uses
it.

It goes no later than the crumb arm itself, whose own deletion is
`the-lancrumbcase-boot-is-a-scout-arm-owed-a-deletion-once-the-step-is-named`.
