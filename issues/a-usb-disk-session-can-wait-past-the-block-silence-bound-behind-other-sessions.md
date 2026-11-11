---
status: open
kind: defect
opened: 2026-11-11
severity: [data-loss]
---

# A USB disk's session can wait past the block silence bound behind other sessions

**Owner.** The storage track: usbd's mass-storage driver and the block
protocol.

The block protocol bounds a server's silence: while it owes a session an
answer it posts a completion, or ends the session, within
`toyos_blockring::SILENCE_BOUND_NS`, and a client ends a session silent
longer. The NVMe driver shows that its figure from a command's submission
fits (`system/nvme/src/nvme.rs`, `ANSWERED_WITHIN`), which is not yet the
whole path (below). usbd cannot as it is:

- One command at a time per device (Bulk-Only Transport 1.0 §5.1), from a
  queue of up to `QUEUE` (32) commands shared by every session on the device
  (`system/usbd/xhci/src/msc.rs`). Each of a command's transfers may take
  `COMMAND_NS` (10 s) and still succeed, so one session's request can wait
  behind other sessions' commands for about 32 × 30 s, 16 minutes, with no
  failure and no completion of its own.
- A break climbs the recovery ladder (`system/usbd/xhci/src/ladder.rs`): class
  reset, port reset and enumeration again, the last reset. Its steps add their
  own bounds, whose sum is not derived.

A client that ends such a session reconnects to the same running driver,
which refuses the open `Held` while it still holds the request, and the
client's disk is gone for its life (`system/diskserver/src/disk.rs`). The
acknowledged writes the client keeps that no flush has covered are then
reported not durable by its next flush: accepted data later reported lost.

The NVMe driver's figure, `ANSWERED_WITHIN`, equals the bound with no slack and
counts from a command's submission to the device. It leaves out:

- the wait to be pulled: a session's request waits behind other sessions'
  requests whenever the device is full (`toyos_blockserve::Sessions::pull`
  takes the sessions in order), and behind a reset, during which nothing is
  pulled;
- the serve loop's lag between a command reaching `COMMAND_SILENCE` and the
  reset;
- the reset's own register and page work;
- posting the reset's answers and ringing the sessions after it;
- the client's clock, which starts just before it publishes.

**Exit.** Each block driver's worst case from a request's publication to its
completion, the wait behind other sessions included, is shown to fit under
`SILENCE_BOUND_NS`, with a host test of usbd's; or the protocol's bound is
redefined so a session's silence does not count other sessions' work, and
both drivers show they keep it.
