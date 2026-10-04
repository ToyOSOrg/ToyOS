---
status: open
kind: defect
opened: 2026-09-29
---

# netd's I219 drops a transmit burst past its ring, and only a retransmit timeout recovers it

`I219::tx` (`userland/netd/src/i219.rs`) writes a frame the driver has no
descriptor for into a scratch buffer and drops it: smoltcp's `TxToken` cannot
refuse. But `Device::transmit` can, by answering `None`, and then smoltcp keeps
the bytes in the socket and sends them on a later poll. netd's `transmit`
(`userland/netd/src/main.rs`) answers `Some` every time. `toyos_i219::TX_RING`
is 16, so 15 frames can be in flight. Every frame after those 15 in one poll is
lost, and TCP gets it back only when its retransmission timeout fires. For
smoltcp 0.12 that timeout starts at 700 ms (`RTTE_INITIAL_RTT` +
4 × `RTTE_INITIAL_DEV`).

## Measured

The T14 run of PR #609 at `7d2e15c0`, boot `lantalkcase`. `logd` admitted the
host's stream at 19.141 s. At 19.180 s netd said `9 frame(s) dropped with no
transmit descriptor free`, and `reboot` came at 19.403 s. The host's
`stream.log` is `kernel.log`'s first 217 lines, which is 21890 bytes, and
15 × 1460 = 21900 falls inside line 218. The run of `main` at `7e151819`
stopped at the same 217th line, with 595 ms between the admit and the stop.
Both windows are shorter than the first retransmit timeout. The same count appears
on the `lanswapcase` boot: 11, 18 and 23 frames dropped.

## Owner

Stage 2 of `issues/the-lan-is-not-yet-production-grade.md`, a gigabit
driver that holds.

## Exit

`transmit` answers `None` when no descriptor is free, and a descriptor coming
free wakes netd's poll. A host test pushes more than `TX_RING` frames' worth
through one poll and asserts that `tx_dropped` stays 0 and every byte leaves.
