---
status: open
kind: defect
opened: 2026-09-26
---

# The NVMe driver resets a drive that is slow and healthy

The NVMe driver reclaims a command the controller has not answered in
`COMMAND_SILENCE`, ten seconds (`system/nvme/src/nvme.rs`), by resetting the
controller. The same ten seconds bound each admin command of the reset, and
with the reset they set the block protocol's silence bound
(`toyos_blockring::SILENCE_BOUND_NS`), which a longer one must raise. NVMe
bounds no command's time, and a Flush of a large volatile cache is the
slowest thing a healthy drive does: a drive still flushing at ten
seconds is reset, every command it held is answered `Device`, and a flush
retried past `toyos_blockring::client::MAX_ATTEMPTS` reaches its caller as
`Device` — a loss reported for a drive that lost nothing.

The number is policy with no measurement behind it: no drive this project runs
on has had its worst Flush timed.

**Exit condition.** The silence bound is derived from the drive (its cache
size and write rate, or a measured worst Flush on the T14's NVMe and on QEMU),
or a silent command is first aborted (NVMe Abort, §5.1) before the controller
is reset; and a guest test holds a Flush past the old bound without the
session hearing `Device`.

Every admin command, at bring-up and in a reset, is held to the same ten
seconds since #1235, where it was `CAP.TO`. The T14 brought its controller up
under it at #1235's head (a `metalcase` boot: up in 2 ms, APST on, DATA served
off the NVMe).

- **Owed on metal:** the AMD bench, the `microcode` and `machine_reboot` rows
  (the `metalcase` image) in one boot of main after #1235. Main must pass both
  rows, and its log must show the nvme driver's `up:` line with no `NOT
  SERVING` and no `was not answered in`, `fileserver: Data serving` off the
  NVMe, and no session's server that broke the protocol, answered nothing in
  the bound, or would not take the session back.
