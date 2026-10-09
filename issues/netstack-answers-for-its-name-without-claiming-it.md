---
status: open
kind: defect
opened: 2026-09-24
---

# netstack answers for its name without first claiming it on the network

`toyos-mdns` (`userland/netstack/mdns/src/lib.rs`) announces `<host>.local`
and answers for it, and does neither of the things RFC 6762 asks of a
responder before it uses a name: probing for it (§8.1) and defending it against
another host's answer (§9). §8.3 announces a unique record that has "completed
the probing step"; this one has not, and each announcement and each multicast
answer carries the cache-flush bit (§10.2), which tells every cache on the link
to drop any other record of the name.

Every machine this tree boots is named `toyos-t14` (`dhcp::HOSTNAME`,
`userland/netstack/src/dhcp.rs`), so two of them on one network both answer for
that name, each flushes the other's record, and a resolver reaches whichever
spoke last.

The record is announced, unprobed, on every new address (the responder that
ships, called by `userland/netstack/src/mdns.rs`) and, in `toyos-net-node`,
which ships in nothing yet, also on every return of the link under a held lease
(`Responder::link_returned`): §8 asks for both steps there, probing first, and
only the announcing is built. What a returning link multiplies is the
occasions: each one is two more multicasts of the unprobed record, a second
apart, so two machines named alike each flush the other's record whenever
either's link returns, where on what ships it happens once an address.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`; probing
is the next stage of `toyos-mdns`, not part of the node's stages.

## Exit condition

`toyos-mdns` probes for its name before it announces it, at start-up and when
its link returns (§8, §8.1), defends it (§9), and gives it up, saying so in its
log, when another host holds it; its tests hold the three probes 250 ms apart,
the announcement only after them, a conflicting answer under the probe taking
the name away, and a link's return probing again before it announces. Or each
machine's name is its own, and a test with two guests on one network finds each
by its name.
