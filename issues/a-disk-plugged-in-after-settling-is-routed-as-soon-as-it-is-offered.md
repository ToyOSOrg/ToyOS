---
status: open
kind: defect
opened: 2026-10-21
---

# A disk plugged in after settling is routed as soon as it is offered

diskserver routes nothing until every disk driver it was started with has
settled (`Known::open`, `system/diskserver/src/main.rs`). After that, a disk
that arrives later is routed from its own offer alone. A stick plugged into a
running machine is one; usbd is already settled when the stick arrives.

A partition GUID is routed to the first disk that offers it. If a second disk
later offers the same GUID, `offer` refuses it on both disks for any later
open, but every session already routed to the first disk stays open. A client
that opens before the later disk's offer arrives is refused as if no disk
carried the GUID. It is not told that a disk is still arriving.

**Exit**: diskserver's host tests show the rule. Either sessions already open
on a GUID that a later offer duplicates are ended, or the open stays routed
and the duplicate is refused, with that choice stated at `offer`. Owner:
diskserver, held by whoever takes up USB hot-plug after #971.
