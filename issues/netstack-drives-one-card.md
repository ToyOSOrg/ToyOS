---
status: open
kind: defect
opened: 2026-10-12
---

# netstack drives one card

netstack opens the first of `CARDS` the supervisor endowed, and no other
(`system/netstack/src/main.rs`, `main`). It opens it behind `Card`, an enum
of its in-process drivers (`system/netstack/src/card.rs`). Its node is one
interface with one DHCP client (`system/netstack/node/src/lib.rs`). So a
machine with a wired and a wireless link uses only one of them. [ip] already
routes over several interfaces, the lower index winning a tie
(`system/netstack/ip/src/lib.rs`).

The owner ruled for several interfaces over one chosen by the manifest. He
also ruled that "netstack runs every link a driver brings, wired first, with
failover" (#876, issuecomment-6120490125, ruling 3).

Owner: P5 of `issues/there-is-no-wifi.md`, after the LAN track's stage 3
moves the drivers out (`issues/the-lan-is-not-yet-production-grade.md`).

**Exit**:
- one interface per link, each with its own DHCP client, lease and mDNS
  responder, under one host name;
- links ranked wired before wireless, then by arrival;
- a guest with two virtio NICs in which the first's link is set off over
  QMP, and traffic leaves on the second.
