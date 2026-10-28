---
status: open
kind: defect
opened: 2026-10-28
---

# A stream's ring never shrinks

A stream region commits its header and 64 KiB of each ring at the connect,
and each ring's producer doubles it, up to 4 MiB, when it needs more
(`toyos::transport::stream`). Nothing gives a page back: a long-lived stream
that once carried a burst keeps the most it ever held, up to 8 MiB + 4 KiB,
for as long as it lives, counted to its client's job. netstack prices each
place at that most (`PLACE_BYTES`, `system/netstack/src/main.rs`).

Owner: the orchestrator, who assigns it after #1077 lands.

Exit: a ring whose stream has gone idle gives back what it no longer holds,
at a quiescent point both ends agree on (both cursors equal, under the
session's gates), through a kernel call that decommits pages of a memory
object; a host test of the ring and a loom model of the shrink beside a
growth and a write, and a guest test that reads the client's account fall
after an idle stream's shrink.
