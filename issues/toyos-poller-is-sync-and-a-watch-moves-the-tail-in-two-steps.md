---
status: open
kind: defect
opened: 2026-10-02
---

# `toyos::Poller` is `Sync`, and a watch moves the submission tail in two steps

`Poller::watch_raw` loads the submission tail, writes the entry at it and
stores the tail plus one, through `&self`, and `Poller` is `unsafe impl Sync`
(`toyos/src/poller.rs`). Two threads watching through one `&Poller` write one
slot at once and advance the tail once, so one watch is lost. `rg 'Arc<Poller>'`
and `rg 'static.*Poller'` over `toyos`, `userland` and `tests` find no poller
shared between threads.

**Exit**: `Poller` is not `Sync`, or a watch claims its slot in one step.
