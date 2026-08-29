---
status: open
kind: defect
opened: 2026-08-29
---

# `bcachefs::Mounted::list` materialises the whole tree before the bound fires

Filed from PR #340's adversarial review; the numbers below are that review's
measurements, re-verified against this tree's constants.

`Mounted::list` (`bcachefs/src/fs.rs:697`) calls `btree::collect_all`
(`bcachefs/src/btree.rs:453`), which materialises one `Entry { key: Key,
value: Vec<u8> }` per non-deleted entry in the tree — 48 bytes per element —
and then builds a second `Vec<(String, u64)>` over it, both unbounded. Only
after both exist does the adapter compare against the limit, and its own doc
comment concedes it: *"Checked after the work, not before:
`bcachefs::Mounted::list` exposes no count to check first"*
(`kernel/src/bcachefs_adapter.rs:211`, both arms).

The bound therefore never gets to refuse the interesting case. Past 32,768
entries, `Vec` doubling requests 65,536 × 48 = 3,145,728 bytes, over
`mm::MAX_HEAP_ALLOC` = 2,093,056 — and `GlobalAlloc` *asserts* there
(`kernel/src/mm/alloc.rs:529`), so the kernel panics inside `collect_all`
before the adapter's `names.len() > limit` (limit: `vfs::MAX_LIST_ENTRIES`,
16,384) ever runs. The second `Vec` crosses the same ceiling at the same
count (65,536 × 32 = 2,097,152). Reachable from `sys_readdir` on a mount
with more names than the cap — ordinary `create` calls on `/home`, no
crafted field needed.

Same class as the closed `read_link` defect one function over (PR #340), and
the residual `issues/isolation/untrusted-input-panics.md` already records in
prose: the trait takes the limit so an implementation can refuse *before* it
allocates, `TmpFs` does, and bcachefs cannot yet. The fix direction is a
counted or bounded walk through the adapter — `collect_all` takes the limit
(or an iterator the adapter counts) and refuses past it before either `Vec`
grows, with the refusal preserved as `ResourceExhausted`.
