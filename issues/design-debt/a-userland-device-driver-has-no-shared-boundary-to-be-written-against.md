---
status: open
kind: finding
opened: 2026-09-08
---

# A userland device driver has no shared boundary to be written against

`toyos-i219` declares `Registers`, `Clock`, `DmaBuffers` and `Interrupts` in its
own `lib.rs`, and netd implements them in `userland/netd/src/i219.rs`. Each has
exactly one real implementation, so what they name is this driver's private
boundary rather than the one every userland driver is written against.

Two of the four are already chip-shaped. `DmaBuffers::read`/`write` deal in
64-bit words because a legacy e1000 descriptor is two `u64` halves, which is not
the shape an xHCI TRB or an NVMe submission entry fits; `Registers` is 32-bit
dwords for the same reason. A second userland driver would either widen them or
declare its own four, and at that point there are two answers to one question.

What is not yet decided is whether there is one boundary at all: whether a
`toyos-userdev` (or a module of the SDK) owns "a mapped register window, a
grant, a monotonic clock and an interrupt record" for every process that drives
a function, or whether each driver crate keeps its own and the shared part is
only netd's `device.rs`. The call is the owner's, and nothing is owed until a
second userland driver exists to be written against it — the virtio NIC beside
`toyos-i219` in netd does not use these traits at all.
