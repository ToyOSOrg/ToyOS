---
status: open
kind: defect
opened: 2026-10-02
---

# A TCP connection that takes a freed slot is queued twice

`Tcp::free` (`toyos-net-shard/tcp/src/stack.rs`) takes a freed connection's index out of `parked` and leaves it in `active`. `transmit` drops a stale index when it pops it and finds the slot empty, but a connection that takes the slot before then is queued by `settle` under the same index, so `active` holds it twice and it has two turns a round. A copy goes when its turn finds nothing due, and the next `settle` queues another while the first is still there, so the second turn lasts for as long as the connection stays busy.

Measured on `toyos-net-tcp`'s own harness (`tests/egress.rs`, `with_second`), with a scratch test that is not committed:

- a connect to 192.0.2.2:81 is queued with no credit and aborted;
- a connect from port 49155 to 192.0.2.3:81 takes its slot and is established;
- 3 × 1460 bytes are queued on it and on the second connection, port 49153;
- one opportunity has credit for six frames.

The source ports left in the order 49155, 49155, 49153, 49155, 49153, 49153. One turn each is 49155, 49153, 49155, 49153, 49155, 49153.

`s_pl_012_a_connection_freed_while_waiting_leaves_no_turn_behind` holds one turn each for a connection freed while parked.

It is `issues/design-debt/toyos-has-its-own-network-stack.md`'s to fix.

Exit condition: a connection freed while queued leaves no index behind in `active`, and a host test of the sequence above sees one turn each.
