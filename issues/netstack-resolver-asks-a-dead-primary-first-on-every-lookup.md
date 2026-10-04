---
status: open
kind: defect
opened: 2026-09-26
---

# netd's resolver asks a dead primary first on every lookup

Every lookup asks the lease's first server first (`toyos_dns::Lookup::start`),
with no memory of that server's silence. On a network whose primary resolver
is down, every lookup spends one whole wait (`toyos_dns::WAIT_MS`, 2 s) on it
before the next server is asked. With `resolve::MAX_LOOKUPS` = 16 slots, that
caps netd at 8 lookups a second, and every lookup past that is refused
`ERR_RESOURCE_EXHAUSTED`.

A second cost comes on top of the first: smoltcp asks for one neighbour's link
address per second for the whole interface (`neighbor::Cache::SILENT_TIME`),
and a query to a primary that answers no ARP takes that second. So the working
server's address is learned a second late, and it is learned again whenever
its cache entry expires (`ENTRY_LIFETIME`, 60 s). A frame from the neighbour
restarts the entry's lifetime, so the entry expires only after 60 s in which
the working server sent nothing.

Measured on smoltcp 0.12's own `Interface` with the host harness of
`userland/netd/src/resolve/tests.rs` (`Net`). Time moved to netd's own wakes.
The servers were `[SILENT, ANSWERS]`: the first on the link and answering no
ARP, the second answering. The scratch test that drove it is not committed.

- **One lookup started every 50 ms for 60 s:**
  - 1201 lookups started, and 737 of them refused `ERR_RESOURCE_EXHAUSTED`;
  - 448 answered, none failed, and 16 still in flight at the end;
  - the first answer came 3000 ms after the first lookup started, and every
    later one 2000 ms after its lookup started;
  - smoltcp asked for the silent server's address 58 times and the answering
    server's once.
- **The same stream for 180 s:** 3601 started, 2195 refused, and the answering
  server's address asked once.
- **Lookups for 10 s, none for 65 s, then lookups again:** the answering
  server's address was asked a second time. The first answer after the pause
  took 3000 ms again, where the ones after it took 2000 ms.

Exit condition: either the resolver keeps a history of each server's answers
and orders the servers by it (RFC 1035 §7.2), so a silent primary is asked last,
or neighbour discovery for one server no longer waits behind another server that
never answers. A host test must show both: a stream against `[SILENT, ANSWERS]`
past its first seconds is answered in less than one `WAIT_MS`, and none of it is
refused.
