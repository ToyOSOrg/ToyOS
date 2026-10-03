---
status: open
kind: defect
opened: 2026-10-03
---

# The netstack stream test helper is included by no test

`tests/toyos-rust-tests/src/netstack_stream.rs` says each netstack stream test
includes it whole, and that its host half is `PatternServer` in
`tests/toyos.rs`. Nothing includes it: no `#[path]`, `mod` or `include!` in the
tree names it, and `PatternServer` is in no file. It compiles in no build, so
its `#![allow(dead_code)]` hides nothing a compiler would ever see.

**Exit**: the file is deleted, or a registered test includes it.
