---
status: open
kind: defect
opened: 2026-09-22
---

# A disk operation can spin past the TLB-ack tripwire before its break

`arch::tlb::ACK_TIMEOUT` (5 s) is held above xHCI's `CALL_AFTER_BREAK`
(4.75 s), "the longest a disk call spins with `IF` clear once its transport has
broken". But the call's bound is measured from the wait that broke, and a USB
disk operation holds the controller lock — `IF` clear — from its first command:
`transfer_blocks` sends one command per `MSC_MAX_BLOCKS` batch, and starts a
batch while the operation's 2 s `block::OPERATION` budget has any left. A batch
that starts at 1.99 s and breaks runs its ladder to 1.99 + 4.75 = 6.74 s after
the operation began, all of it with `IF` clear: past the tripwire on any CPU
waiting for this one's TLB acknowledgement. That is arithmetic on the declared
constants; no boot has been seen to do it.

## Exit condition

The whole of one operation's `IF`-clear spin is under `ACK_TIMEOUT` by
construction — the call bound opens where the operation does, or a later batch
starts only with a whole call's bound still inside it — and a staged boot in
which the first batch spends most of the budget and the next one breaks shows
the operation ending inside it.
