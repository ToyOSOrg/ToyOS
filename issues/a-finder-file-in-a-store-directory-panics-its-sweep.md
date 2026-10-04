---
status: open
kind: defect
opened: 2026-09-30
---

# A Finder file in a store directory panics its sweep

`keystore::sweep_by` takes every entry of a store directory as `<key>` or
`<key>.<suffix>`, so a `.DS_Store` the macOS Finder writes there names the key
`""`, and `buildlock::keyed_idle` then opens the lock directory itself:
`build lock: open <primary>/.git/toyos-build-locks/llvm/: Is a directory (os
error 21)`. It happened after an LLVM placement, whose product was whole, so
the next build went on; the sweep had removed nothing.

**Exit**: a sweep passes over a store entry that names no key, with a test.
