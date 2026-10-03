---
status: open
kind: defect
opened: 2026-09-24
---

# netd answers for its name without first claiming it on the network

`userland/netd/src/mdns.rs` answers multicast DNS for `toyos-t14.local` and
announces it on every new lease, and skips what RFC 6762 asks before a
responder uses a name: probing for it (§8.1) and defending it against another
host's answer (§9). Every machine this tree boots asks its network for the one
name `toyos-t14` (`dhcp::HOSTNAME`), so two of them on one network both answer
for it, and a resolver may reach either.

## Exit condition

netd probes its name before it answers for it and gives it up — saying so in
its log — when another host holds it; or each machine's name is its own, and a
test with two guests on one network finds each by its name.
