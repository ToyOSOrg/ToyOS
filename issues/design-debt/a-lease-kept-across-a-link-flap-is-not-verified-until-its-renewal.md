---
status: open
kind: defect
opened: 2026-09-23
---

# A lease kept across a link flap is not verified until its renewal

netd keeps a bound DHCP lease when the link goes down and comes back
(`userland/netd/src/main.rs`'s main loop, `dhcp::restart`'s header): the
client restarts discovery only on a link that comes up with no lease held. The
client offers no way to renew early, so the lease is next checked against a
server at its own renewal time, T1 (RFC 2131 §4.4.5), which is half the lease
by default.

Until then a cable moved to another network keeps the old address, route and
resolvers. Every frame the machine sends in that window goes out under an
address the new network never leased it. The compromise was chosen over the
alternative the client offers, a restart, which gives the address up before it
asks again and so takes a machine whose cable only flapped off its network for
a whole exchange.

## Evidence

`lan_lease_report` (`tests/common/lan.rs`) takes QEMU's link away after the
lease and gives it back, and passes only if the report records no second
`leased` line and no `lost` line after the flap: the old lease is kept and no
server was asked about it.

## Owner

The successor of the I219 PHY branch (PR #453) on the LAN track, "The LAN
reaches a router, and is not yet production grade" (stage 4, the stack).

## What would close it

The DHCP client verifies a kept lease when the link comes back: a renew-now
request, or RFC 2131 §3.2's INIT-REBOOT (a DHCPREQUEST for the address it
holds), with the lease kept while the answer is outstanding and given up on a
DHCPNAK. `lan_lease_report` then sees the request after the flap.
