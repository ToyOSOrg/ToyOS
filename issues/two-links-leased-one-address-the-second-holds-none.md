---
status: open
kind: defect
opened: 2026-11-01
---

# Two links leased one address: the second never holds one

netstack runs every link a driver brings (`issues/there-is-no-wifi.md`, P5),
and [ip] keeps an address unique across its interfaces: a second interface
adding an address the first holds is refused `addr.duplicate`. Two
separate networks can lease the same address, two home routers each handing
out 192.168.1.10, and then the second link's probe is refused, its client
counts `node.address-refused` and starts its exchange over, and the same
server offers it the same address again: the second link never holds an
address, its client asks for the boot, and its DHCP lines repeat.

Nothing breaks the first link, and the uniqueness rule is what keeps
`toyos-net-tcp`'s `lose_address` and the strong host model's source
selection sound: one address names one interface.

**Owner**: the orchestrator.

**Exit**: the second link's client declines the address (RFC 2131 §3.1, a
DECLINE) and asks for another, as it would for one ACD found in use, so that
the second link leases a different address where its server has one; with a
host test of two links offered one address.
