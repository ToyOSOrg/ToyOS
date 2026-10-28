---
status: open
kind: defect
opened: 2026-10-12
---

# netstack drives one card

netstack opens the first link a NIC driver brings, and no other: a hello with
another address, or with that address while its link is alive, is refused
`OneLink` (`system/netstack/src/main.rs`'s loop).
Its node is one interface with one DHCP client, made with the first link's
address (`system/netstack/node/src/lib.rs`). So a machine with a wired and a
wireless link uses only one of them, whichever driver says hello first.

Because the node needs that address, netstack serves nothing before a
driver's hello: every client waits for it, and on a machine with no NIC
driver for 30 s (`LINK_BOUND`, `system/netstack/src/main.rs`), after which
netstack leaves and a client hears it gone. [ip] already
routes over several interfaces, the lower index winning a tie
(`system/netstack/ip/src/lib.rs`).

The owner ruled for several interfaces over one chosen by the manifest. He
also ruled that "netstack runs every link a driver brings, wired first, with
failover" (#876, issuecomment-6120490125, ruling 3).

That reverses part of `issues/first-match-selection-that-remains.md`
(`kind: rejected`), whose virtio bullet says a second virtio NIC "would be
dropped" and that "Neither is a defect today". Under ruling 3 a second NIC
dropped is this defect.

Owner: P5 of `issues/there-is-no-wifi.md`. The drivers are out: each is a
process of its own over a link (`toyos-link`).

**Exit**:
- one interface per link, each with its own DHCP client, lease and mDNS
  responder, under one host name, and a node that serves before any link,
  so `LINK_BOUND` goes;
- links ranked wired before wireless, then by arrival;
- a guest with two virtio NICs in which the first's link is set off over
  QMP, and traffic leaves on the second;
- the NIC taken out of the virtio bullet of
  `issues/first-match-selection-that-remains.md`.
