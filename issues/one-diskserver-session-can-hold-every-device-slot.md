---
status: open
kind: defect
opened: 2026-09-26
---

# One block session can hold every device slot

A disk driver serves its sessions through `toyos_blockserve::Sessions`
(`lib/blockserve/src/sessions.rs`), whose `pull` takes what a session has
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

**Exit condition.** `Sessions` bounds what one session holds on the device by
a share of the device's slots, and a host test of `pull` over a device model
with fewer slots than `DEPTH` runs two busy sessions, reads each one's peak
outstanding commands, and goes red with the rule deleted.
