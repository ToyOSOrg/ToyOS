---
status: open
kind: defect
opened: 2026-10-28
---

# A stop's file sync, and its failure, are never recorded

`Supervisor::halt` (`system/supervisor/src/stopping.rs`) has `logkeeper` make
the log whole, then syncs every writable file server (`sync_files`), then
stops the drivers and asks the kernel to stop. Whatever `sync_files` and the
drivers' stop say — a server that would not sync, one that did not answer in
`toyos_quiesce::SYNC_MS`, a driver's shutdown not complete in its bound — is
said after the log's last write, so it reaches the kernel's ring and no
file: a boot's readback can never tell a stop that made DATA durable from
one that lost it. The partitions work's first designation on the AMD bench
(PR #1085) formatted 600 GiB at boot with 4,803 blocks dirty when the
reboot began, and whether they reached the medium was not observable in its
readback; it took the next boot's mount to say.

**Owner:** the supervisor's stop and `logkeeper`.

**Exit condition:** a stop's sync outcome for each file server, and each
disk driver's shutdown outcome, are on the log partition when the machine
stops — the log made whole after them, its own server synced last — and a
guest test stops a machine whose DATA server will not sync and reads the
refusal off the log the next boot sees.
