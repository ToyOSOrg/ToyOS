---
status: open
kind: defect
opened: 2026-09-26
---

# blockd resets a drive that is slow and healthy

blockd reclaims a command the controller has not answered in
`COMMAND_SILENCE`, ten seconds (`userland/blockd/src/main.rs`), by resetting the
controller. NVMe bounds no command's time, and a Flush of a large volatile
cache is the slowest thing a healthy drive does: a drive still flushing at ten
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
