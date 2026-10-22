---
status: none
kind: rejected
opened: 2026-09-26
---

# The block service does not refuse the running ROOT

diskserver serves the partition the loader read ROOT from like any other:
the grant its connector was minted with decides whether a session opens it
and whether that session writes. It is not told which partition the machine
runs from and refuses nothing for being that partition.

The owner declined a blanket refusal on 2026-10-20. An authorized write there
can invalidate the stored image and leave the installation unbootable; that
is the grant holder's authority, and no automatic rollback is assumed to
recover it — the update policy is fix-forward.
