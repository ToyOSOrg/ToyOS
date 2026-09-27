---
status: expected-red
kind: defect
opened: 2026-08-08
---

# `usb_disk_index_stable` reds 1 of 5 on CI: nothing enumerated on the first controller

From the twelve-shard CI probe (`probe-rate.yml` run `31258202923`, tree
`f8f73e1`, five reps of the exact `ci.yml` configuration): `usb_disk_index_stable`
red 1 of 5, shard 2, `Sched::Parallel`, `nothing enumerated on the first
controller`. Re-taken 2026-09-04 against the last three nightly `ci` runs on
`main` (`33485669019`, `33603832656`, `33728852421`): of the eleven names that
probe found, only this one still reds.

Split out of `issues/hardware/eleven-names-red-on-ci.md`, which covers eleven
names and has no exit condition for this one in particular.

**Exit condition.** Re-enabled when a reproduction pins whether the first
controller's device is genuinely not there yet at enumeration time (a boot
ordering question) or the enumeration itself missed a device that was, with
the controller's own register state read at the failing enumeration. Owner:
the USB storage index path (`tests/common/usb.rs`'s
`usb_disk_index_stable` and `kernel/src/drivers/xhci`); nobody is holding it
yet.
