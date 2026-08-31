---
status: open
kind: defect
opened: 2026-08-31
---

# `delete_prefix` is the one caller left on `btree::collect_all`'s unbounded walk

`bcachefs/src/btree.rs:454`'s `collect_all` is `collect_up_to(io, root,
usize::MAX)` — no ceiling, one entry per live leaf materialised before
anything is checked. `Mounted::list` used to call it too, and #343
(`09250c97`) moved `list` onto `collect_up_to` with the caller's real limit
(`bcachefs/src/fs.rs:700-701`) for exactly the reason this file exists: past
the tree's own bound the doubling `Vec` panicked the kernel via
`mm::MAX_HEAP_ALLOC`.

`delete_prefix` (`bcachefs/src/fs.rs:814-815`) still calls `collect_all`
directly, and it is now the *only* caller — confirmed by `rg -n
"collect_all" bcachefs/`, one production call site left, at `fs.rs:815`. It
also has zero callers of its own in `kernel/`, `src/`, or `userland/`
(`rg -n delete_prefix kernel/ src/ userland/` returns nothing); the only
callers are `bcachefs/src/fs.rs`'s own tests and
`bcachefs/tests/integration.rs`.

Same shape #343 removed from `list()`, left standing here because nothing in
the shipped tree reaches it — yet.

Exit: bound `delete_prefix` through `collect_up_to` with a real ceiling the
way `list` was fixed, or delete `delete_prefix` along with its dead caller
surface if nothing needs it.

Provenance: adversarial review of PR #343.
