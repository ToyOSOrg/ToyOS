---
status: open
kind: tooling
opened: 2026-09-22
---

# The T14's lan judge owes netd records the stick never carries

`tests/common/lan.rs::on_metal` (the `lan_dhcp_lease` metal row) refuses a
readback without netd's `MAC`, `LINK_UP`, `READY` and `LEASE` records, and its
module header says those reach the stick because netd's `say!` is a write to a
console object.

It is not. A console-object write goes to `drivers::serial` and the serial
backend (`ConsoleObject::write` → `ConsoleLine`), never into the record ring
that `logd` persists; on the T14 that backend is `Backend::None`. Metal run 57's
kernel log carries no `netd:` line at all, `netd: MAC …` included, which netd
writes on every boot that opens a card.

So the lancase judge cannot pass on the T14 whatever the network does, and a
failure it reports for the missing records says nothing about the card.

This closes when the lan judge reads only what a T14 boot can persist, or netd's
records reach the ring `logd` reads.
