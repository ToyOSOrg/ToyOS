---
status: open
kind: defect
opened: 2026-08-26
---

# `toyos-abi/src/ring.rs`'s doc comment still names `kernel/src/io_uring.rs`

The 2026-08-20 internal-vocabulary pass renamed `kernel/src/io_uring.rs` to
`kernel/src/inbox.rs`. `toyos/src/poller.rs`'s citations of the old path have
since been fixed; `toyos-abi/src/ring.rs:10`'s has not:

```rust
//! rule `kernel/src/io_uring.rs` states for its own tail.
```

A one-line doc fix (`io_uring.rs` -> `inbox.rs`), but it sits under
`toyos-abi/src`, so per `CLAUDE.md` it costs a sysroot claim and may not ride
a non-ABI PR. Split out of
`issues/build/retired-inbox-op-names-are-a-spelling-behind.md` (closed
2026-08-26) when the rest of that issue's fix landed without touching the
sysroot crates.
