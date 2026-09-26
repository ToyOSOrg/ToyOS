---
status: open
kind: defect
opened: 2026-09-26
---

# A poll registered racing its own process's close outlives the release

`kernel/src/inbox/mod.rs`'s `process_watch` resolves the handle, clones the
object out, and only then `add_poll`s onto the object's watch with no lock
held. A PCI function's watch is a per-slot `static`
(`kernel/src/pcidev/mod.rs`'s `WATCHES`), and the release's `tear_down` answers
every poll on it with `cancel_polls`. A sibling thread of the same process that
closes the claim between the resolve and the `add_poll` lets the poll land on
the slot's watch *after* that cancel: the slot's next holder's first interrupt
then fires it into the old process's ring — one bit of another process's device
activity, across the isolation boundary.

It predates the watch: main's `inbox::add_watcher` pushed onto
`pcidev`'s per-slot watcher list in the same order.

## Exit condition

A poll cannot reach a watch whose object has ended — the registration rechecks
the object's life after its `add_poll` and answers itself as gone — with a
guest test that closes a claim from one thread while another submits
`OP_WATCH` on it, and a second process claiming the slot afterwards sees no
completion arrive in the first one's ring.
