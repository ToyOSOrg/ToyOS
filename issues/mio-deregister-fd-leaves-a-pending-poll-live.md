---
status: open
kind: defect
opened: 2026-08-16
---

# mio's ToyOS selector deregisters a token but not the kernel's watch on it

`src/sys/toyos/selector.rs` in the mio fork (pinned at `9f34d36c`,
`Cargo.lock`) keeps its own registration list rather than asking the kernel to
track interest:

```rust
pub fn deregister_handle(&self, handle: RawHandle) -> io::Result<()> {
    let mut inner = self.inner.lock().unwrap();
    inner.registrations.retain(|r| r.0 != handle);
    Ok(())
}
```

This removes the entry from `SelectorInner::registrations`, so `select()` stops
queuing an `OP_WATCH` for it on later calls: its re-arm loop iterates only what
is still in the list. But a watch already submitted for that handle in an
*earlier* `select()` is not touched: nothing here cancels it, and nothing waits
for it to land. If the kernel completes that stale watch after
`deregister_handle` returns, the completion still carries the old token, and
the next `select()`'s drain loop does not check the token against the current
registration list before turning it into an `Event`. The caller (tokio's
reactor, or anything else built on mio's `Registry`) gets one readiness
notification for a resource it was promised was gone.

## Why there is nothing to cancel with

There used to be an ABI op for this. `sdk/abi/src/inbox.rs`'s op code 2 was
`IORING_OP_POLL_REMOVE`, retired in PR #89 (`c41b831`, "abi: four names
retired, and the number each one held") as caller-less, on the reasoning that
a poll the kernel takes is one-shot and consumed by the completion it posts.
That is true of the watch itself, but says nothing about *when* the completion
is consumed relative to `deregister_handle`, and the selector shows the two are
not synchronized. The retirement established that mio never calls a remove op,
not that nothing needs one.

## What a fix has to promise

A kernel wait a kill ends is answered `Cancelled` (`kernel/src/watch.rs`).
mio's selector is the userland mirror of that, and a `deregister` that cannot
promise "no event after this point" is what it lacks: no inbox op withdraws a
watch (`sdk/abi/src/inbox.rs` has `OP_NOP`, `OP_WATCH` and `OP_ACCEPT`).
Re-adding a cancel op, or otherwise making `deregister_handle` synchronous with
the kernel's pending state (draining the inbox for that handle, or the drain
loop dropping a token no registration holds), is in scope for whoever fixes
this; retiring a remove op a second time without addressing this would repeat
the same reasoning gap.

Filed rather than fixed: this is a fork (mio, not this repository) and a design
question (what the cancel primitive should look like), not a local bug fix.
