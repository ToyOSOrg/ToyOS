---
status: open
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

`usb_disk_index_stable` is deleted; `4505f872d`'s parent restores it.

**Exit condition.** The cause of the empty first controller is fixed. Owner: orchestrator.
