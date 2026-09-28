---
status: open
kind: defect
opened: 2026-09-28
---

# `clean`'s deps-stamp write is swallowed

`src/build.rs`'s `clean` ends with `fs::create_dir_all(&target).ok();` and
`fs::write(target.join(".deps-stamp"), fingerprint).ok();` — the two arms
above them (`cargo clean` and the per-target `remove_dir_all`) both panic on
failure, so these are the only swallowed errors left in the function. A stamp
write that fails is not reported, and `stale` reads it back as absent or
mismatched, so every later build re-decides `clean` was owed and pays a full
clean it does not need.

**Owner:** whoever next touches `clean` or `stale` in `src/build.rs`.

**Exit condition:** both calls panic on error like their two siblings, or the
`.ok()` is replaced with a call that folds a write failure into the same
refusal `stale` would otherwise silently manufacture.
