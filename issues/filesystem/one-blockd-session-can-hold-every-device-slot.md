---
status: open
kind: defect
opened: 2026-09-26
---

# One blockd session can hold every device slot

blockd's `pull` (`userland/blockd/src/main.rs`) takes what a session has
published while the session's completion ring and the controller have room.
The only per-session bound is the ring's: what is on the device and what is
posted unread stays below `toyos_blockring::layout::DEPTH`, so one session
holds at most 63 commands. Nothing divides the controller between sessions,
so on a controller with fewer I/O slots than that — or with several sessions
that together publish faster than it answers — one client that publishes as
fast as it can keeps the others' requests waiting behind its own.

On QEMU's controller, four I/O queues of 63 commands, the ring's cap is
already below any share a rule would give fewer than five sessions, so a
rule there changes nothing that a test can see.

**Exit condition.** blockd bounds what one session holds on the device by a
share of the controller's slots, and a guest test runs two busy sessions
against a controller with one I/O queue (QEMU's NVMe with `max_ioqpairs=1`),
reads each partition's peak outstanding commands off QEMU's trace by LBA, and
goes red with the rule deleted.
