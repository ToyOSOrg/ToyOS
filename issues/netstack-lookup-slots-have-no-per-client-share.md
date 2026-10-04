---
status: open
kind: defect
opened: 2026-09-26
---

# netd's lookup slots have no per-client share

`resolve::MAX_LOOKUPS` (`userland/netd/src/resolve.rs`) bounds the lookups in
flight across every client at once. One holder of `netd` that asks for names
its servers answer slowly, or not at all, holds all of them for up to a
lookup's whole schedule each, and every other program's lookup is refused with
`ERR_RESOURCE_EXHAUSTED` meanwhile.

netd cannot share the slots out itself: a client is a connection, a program
opens as many as it likes, and netd holds no word for the program behind one
(there is no pid-as-authority, by design). The same is true of piped
connections, bounded by `max_piped_connections` across all clients.

Exit condition: a lookup is charged to something a program cannot multiply by
opening connections (a per-program grant from init, or a kernel-side
connection quota), and a test shows one program at its share leaves another's
lookup answered.
