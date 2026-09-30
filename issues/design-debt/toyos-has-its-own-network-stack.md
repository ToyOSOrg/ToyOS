---
status: open
kind: track
opened: 2026-09-27
---

# ToyOS has its own network stack

netd runs smoltcp. Its replacement is ToyOS's own stack, built clean-room: readers specify each stage from the RFCs, writers build from those specifications and the RFCs alone and never open another stack's source. The specifications live outside the tree; every test names its scenario id, and the review checks each scenario has one.

- Stage 0: the acceptance bar, on smoltcp.
- Stage 1: `toyos-net-wire`, in the tree.
- Stage 2: `toyos-net-tcp`.
- Stage 3: `toyos-net-ip`, `toyos-net-udp`, `toyos-dhcp`.
- Stage 4: `toyos-net-shard` and `toyos-net-testnet`.
- Stage 5: netd on one shard, pipe ABI unchanged; smoltcp leaves netd, `Cargo.toml` and `userland/Cargo.lock` in the same PR.
- Then multi-core, netring (blocked on the owner's ABI ruling), TCP and IP hardening, IPv6, offloads, soak.

The listener defects are this track's: `issues/hardware/a-handshake-nobody-finishes-holds-a-listeners-port-shut.md` and `issues/hardware/a-connect-between-two-accepts-is-reset.md`, on smoltcp until stage 5, and `issues/hardware/an-accept-that-never-reaches-netd-strands-its-listener.md`, in std's accept.

Owed from the stage 3 specifications: OUT-07 by stage 4, whose scheduler chooses between a flow's segment and [ip]'s own frames; US-57 by stage 5, whose netd maps UDP's refusals onto the pipe ABI; `dhcp.renew-unroutable` by stage 5, whose netd counts it where `toyos-net-udp` refuses the renewal `udp.no-route`; FRA-01–20 and PMTU-01–06 by IP hardening.

What stage 3 departs from its specifications:

- US-43 expects `ip.not-for-us` for a datagram to an unjoined group in a frame to that group's MAC; the frame filter refuses it first, as `eth.not-for-us` (`wire.md` §3.3, ETH-23). Exit: the scenario names the frame filter.
- DH-64 expects an announce request from the client on link-up with a lease held; `toyos-net-ip` announces the held address itself on link-up (IP-D6, C-4), so the client asks nothing. Exit: the scenario drops the request, or [ip] stops announcing on its own.
- `udp-dhcp.md` §U9 (3) lets every datagram a closed socket had accepted leave; `toyos-net-udp` holds at most one socket's queue of them together and refuses the rest `udp.tx-discarded-on-close`, because architecture §3.3 holds nothing without a bound. Exit: the specification bounds them.

What stage 3 does not yet meet:

- Its only oracles are the readers' own: the specifications' scenarios and byte vectors, and the tests' own RFC 1071 sum. No behaviour of `toyos-net-ip`, `toyos-net-udp` or `toyos-dhcp` is checked against anything the readers did not write. Exit: exchanges captured from slirp and the T14 (ARP, DHCP, ICMP, IGMP) replay through them at stage 5 and match.

What `toyos-net-wire` does not yet meet:

- Its builders write front to back, not into prefix room ahead of a payload already in a DMA slot. Exit: netd's transmit path builds its headers into that prefix room.
- Its structured fuzz runs bounded in the PR gate only. Exit: the nightly runs it long.
- Its corpus is the specification's vectors alone, so no header layout has an oracle independent of the reader. Exit: frames captured from slirp and the T14 join the corpus.
- It departs from the wire specification: RT-06 sweeps frames and datagrams only, because a message-only vector has no length of its own to be cut against. Exit: the specification covers it.

Exit: `rg smoltcp` is empty outside `issues/`.
