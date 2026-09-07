---
status: open
kind: defect
opened: 2026-09-07
---

# Every wait in the xHCI driver checks its deadline only when the event ring is empty

`USB_TIMEOUT_NS` is 2 s and `block::OPERATION` is the same 2 s by declaration
(`kernel/src/drivers/xhci/mod.rs:293`). Neither bounds what it is written as if
it bounds, because in all four waiting loops the deadline test sits **inside the
`next_event() == None` arm**:

```rust
// kernel/src/drivers/xhci/wait/mod.rs:312, wait_transfer
loop {
    let Some(event) = self.next_event() else {
        if crate::clock::nanos_since_boot() >= deadline { return Err(Quiet::Elapsed); }
        if port.is_some_and(|p| !self.read_portsc(p).connected()) { return Err(Quiet::Gone); }
        core::hint::spin_loop();
        continue;
    };
    ...
    // Not this wait's event: forwarded so a bound device's own
    // interrupt ring stays fed rather than dropped here.
    self.dispatch_event(event);
}
```

**A ring that keeps producing events is a wait with no bound at all.** The same
shape is `wait_command` (`wait/mod.rs:262`); `XhciController::poll`
(`mod.rs:1251`) and `settle_outstanding` (`wait/mod.rs:244`) drain the ring with
no deadline in them at any point.

It is a feedback loop and not merely a drain: `dispatch_event` requeues a HID
interrupt-IN endpoint on every successful completion (`mod.rs:835`), so a device
that completes rather than NAKing keeps the ring non-empty on its own.

What a CPU inside one of these holds is `XHCI` — a ticket spinlock, preemption
off for its whole life — and, on the storage path, the block `Handle` above it,
which `with_disk` takes for the *entire* multi-transfer SCSI command
(`mod.rs:1533`). Both are what `logd`'s `fsync` to the same stick needs. And
the loop polls no TLB shootdown, unlike `Lock::lock`'s spin
(`kernel/src/sync.rs:129`).

So the failure this produces is: one CPU spinning forever with no timeout, no
tripwire and no panic, holding the two locks the log file's own path needs —
which is indistinguishable on a stick from a kernel that stopped. It is the
best structural candidate for
`issues/hardware/a-t14-boot-wedges-after-a-jobs-exit-and-nothing-said-why.md`
and it is a defect whether or not it is that one.

The fix is to test the deadline at the top of the loop rather than in one arm
of it, which costs nothing on a healthy path — the call already runs once per
empty-ring iteration — and turns an unbounded region into the
`no answer in the … in 2000 ms` transport break the driver already recovers
from. `XhciController::poll` and `settle_outstanding` need a bound of their own.

**Exit condition**: an actuator that keeps the event ring producing while a
transfer is outstanding, and a boot in which the wait ends by name inside
`USB_TIMEOUT_NS` instead of never; and a mutation reverting the check that
reds it.
