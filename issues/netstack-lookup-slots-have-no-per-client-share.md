---
status: open
kind: defect
opened: 2026-09-26
---

# netstack's lookup slots have no per-client share

`toyos_dns::MAX_LOOKUPS` (`system/netstack/dns/src/lib.rs`, held by
`system/netstack/node/src/resolve.rs`) bounds the lookups in flight across every
client at once. One holder of `netstack` that asks for names
its servers answer slowly, or not at all, holds all of them for up to a
lookup's whole schedule each, and every other program's lookup is refused with
`ERR_RESOURCE_EXHAUSTED` meanwhile.

A client is a connection, and a program opens as many as it likes. The one
word netstack holds for the program behind one is its connector's badge, the
row and session the supervisor minted it for (`system/netstack/src/serve.rs`),
and anyone can multiply sessions, since every ssh login opens one
(`issues/netstack-replaces-one-programs-cached-answers-to-keep-anothers.md`).
The same is true of the node's places (`system/netstack/node/src/places.rs`), one number across all
clients.

Exit condition: a lookup is charged to something a program cannot multiply by
opening connections (a per-program grant from init, or a kernel-side
connection quota), and a test shows one program at its share leaves another's
lookup answered.
