---
status: open
kind: tooling
opened: 2026-09-25
---

# Harness verdicts read after a flat half-second drain

Several guest tests drain the console for a fixed 500 ms and then judge what
arrived, or read a file QEMU finishes only at its exit — a wait on nothing,
which a slow host turns into a wrong verdict: `tests/common/audio.rs`
(`doom_sound_flood`'s wav read), `tests/common/hda.rs` (three sites),
`tests/common/iommu.rs` (two), `tests/common/pkg.rs`, `tests/common/usb.rs`
and `tests/common/volumes.rs` — every `drain_serial(Duration::from_millis(500))`
in `tests/common/`.

`soundd_log_stall` reads its wav after `QemuInstance::await_exit`, which waits
on QEMU's console closing; that is the pattern.

## Exit condition

`rg 'drain_serial\(Duration::from_millis' tests/common` finds nothing: each
site waits on the event it needs — a line, or QEMU's exit — bounded by a
ceiling that fails loudly.
