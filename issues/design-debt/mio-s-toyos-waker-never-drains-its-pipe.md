---
status: open
kind: finding
opened: 2026-09-26
---

# mio's ToyOS waker never drains its pipe

`src/sys/toyos/waker.rs` on the mio fork (`ToyOSOrg/mio`, branch `toyos`)
writes a byte to a pipe per `wake()` and ignores a full pipe, and the
selector registers the read end under the waker's token; nothing in
`src/sys/toyos/` ever reads that pipe. `toyos::wake` is the wake pipe the
tree now has once (a `Bell` whose `take` empties it), and logd, soundd and
`window::Waiter` use it; mio could not take it as a swap, because draining is
the selector's to do on the waker's token and the selector has no such step.

Not shown: whether a registration left readable makes the selector report the
waker's token on every poll, which is what an undrained pipe would do to a
level-triggered watch, or whether a full pipe ever loses a wake.

**Exit**: the selector takes the bell when the waker's token comes up, through
`toyos::wake`, with a guest test that wakes a tokio runtime more times than the
pipe holds.
