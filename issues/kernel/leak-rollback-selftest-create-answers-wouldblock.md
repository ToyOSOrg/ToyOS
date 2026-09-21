---
status: expected-red
kind: defect
opened: 2026-09-03
---

# `leak_rollback_selftest`'s `create` answered `WouldBlock` once under a loaded host

A full fast tier on `w5b13-gop-mode` at 2d9ebb5c, 2026-09-03: 303/304 in
507.6 s beside other worktrees' guests, `ALONE leak_rollback_selftest: GREEN`.
The diff carried no kernel file.

```
leak-selftest: fat-reopen skipped, create failed: WouldBlock
```

A `create` answering `WouldBlock` is the shape
`issues/kernel/ftruncate-answers-wouldblock-and-nothing-retries-it.md` records
for `ftruncate` — a sibling site, not this one.

Owed: the site in the create path that lets `WouldBlock` reach the caller, and
whether it retries. `src/redlist.rs` quarantines the name until then.
