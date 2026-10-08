---
status: open
kind: defect
opened: 2026-10-08
---

# mDNS answers a unicast-response query with ID zero

`toyos-mdns` (`userland/netstack/mdns/src/lib.rs`, `Responder::answer`) answers
a query from port 5353 that sets the unicast-response bit (RFC 6762 §5.4) at
the asker's own address with the message a multicast carries, ID zero included,
whatever ID the query had. RFC 6762 §18.1 has a unicast response made for one
particular query carry that query's ID. Only the legacy path (§6.7) echoes the
ID today.

Read, not measured against a resolver: found in review of the node's name
(`userland/netstack/node/tests/name.rs`), whose
`an_answer_goes_to_the_group_or_to_the_asker_as_the_query_asked` asks that
query with ID zero and so cannot tell.

Exit condition: that test asks the unicast-response query with a nonzero ID and
finds it in the answer, and `toyos-mdns`'s own test of the bit does the same.
