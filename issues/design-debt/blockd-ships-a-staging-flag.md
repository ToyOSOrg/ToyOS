---
status: open
kind: defect
opened: 2026-09-26
---

# blockd ships a staging flag

`--silence-write <n>` (`userland/blockd/src/main.rs`, read in
`userland/blockd/src/nvme.rs`'s `reap_queue`) makes the shipped blockd withhold
the device's answer to a session's `n`th write. It is how the reset and crash
tests stage a write the device did and nobody was told of, on a device that
always answers, and it is in the binary every image carries; netd's argv
actuators are the same shape.

**Exit condition.** The withholding lives where only a test image has it — a
build of blockd the harness asks for, or a device that can be made to stay
silent (a QEMU fault injection the harness arms) — and the shipped blockd
parses no staging argument.
