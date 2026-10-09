---
status: open
kind: defect
opened: 2026-09-26
---

# netd's resolver asks a dead primary first on every lookup

Every lookup asks the lease's first server first (`toyos_dns::Lookup::start`),
with no memory of that server's silence. On a network whose primary resolver
is down, every lookup spends one whole wait (`toyos_dns::WAIT_MS`, 2 s) on it
before the next server is asked. With `toyos_dns::MAX_LOOKUPS` = 16 slots, that
caps netstack at 8 lookups a second, and every lookup past that is refused
`ERR_RESOURCE_EXHAUSTED`.

The second cost this issue recorded, one neighbour request a second for the
whole interface, left with smoltcp: [ip] asks for each next hop on its own
(`rfc_4861_7_2_2_a_silent_next_hop_holds_up_no_other`,
`toyos-net-ip/tests/nud.rs`), and a query whose resolver's link address [ip]
gives up is let go at once
(`a_query_for_a_resolver_ip_has_given_up_ends_its_lookup_in_the_opportunity_that_would_have_carried_it`,
`userland/netstack/node/tests/resolve.rs`). The first cost is read from the
constants and not measured on the node: [ip] gives a silent next hop up after
3 s, past the lookup's own wait, so a silent primary still costs each lookup
one `WAIT_MS`.

Exit condition: either the resolver keeps a history of each server's answers
and orders the servers by it (RFC 1035 §7.2), so a silent primary is asked last,
or neighbour discovery for one server no longer waits behind another server that
never answers. A host test must show both: a stream against `[SILENT, ANSWERS]`
past its first seconds is answered in less than one `WAIT_MS`, and none of it is
refused.
