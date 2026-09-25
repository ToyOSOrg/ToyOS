---
status: open
kind: defect
opened: 2026-09-25
---

# A faulted claim answers its interrupt read as quiet, so its holder drives a dead function in silence

`kernel/src/pcidev/mod.rs` says a claim whose function the unit refused
"answers every later call `Io`", and `userland/netd/src/main.rs`'s `begin_pass`
dies on exactly that refusal. But the read netd makes on every pass —
`kernel/src/object/ops.rs`'s `PciFunction` arm, through `pcidev::take_record` —
never asks whether the claim faulted, and nothing wakes a holder parked on the
claim when it does. The fault clears bus mastering, so no message ever arrives
again: the holder reads "no interrupt" for ever.

T14 run 132, `lanswapcase` on `wt/toyos-logd` `b9435b98`, and on `main` too:
after the DMA fault in
`issues/kernel/a-function-nothing-resets-faults-its-next-holder-on-its-first-grant.md`
the replacement netd ran on. It never took a message (no `pcidev: slot 0 took
its first message` after the swap), saw its link only on a pass its own timer
woke (`link up ... 10001 ms after the driver came up`), leased nothing (`DHCP:
no lease as toyos-t14 in 20 s`), said `ready`, and init put it in service. The
machine was unreachable until the test runner's 60 s bound rebooted it.

**Exit condition**: a claim whose function faulted refuses its holder's next
interrupt read and wakes a holder waiting on it, gated by a QEMU arm in which
netd's device faults and netd says its claim refused the read.
