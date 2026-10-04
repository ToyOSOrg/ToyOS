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

The listener defects are this track's: `issues/hardware/a-handshake-nobody-finishes-holds-a-listeners-port-shut.md` and `issues/hardware/a-connect-between-two-accepts-is-reset.md`, on smoltcp until stage 5, and `issues/hardware/an-accept-that-never-reaches-netstack-strands-its-listener.md`, in std's accept.

Owed from the stage 3 specifications: US-57 by stage 5, whose netd maps UDP's refusals onto the pipe ABI; `dhcp.renew-unroutable` by stage 5, whose netd counts it where `toyos-net-udp` refuses the renewal `udp.no-route`; FRA-01–20 and PMTU-01–06 by IP hardening.

What stage 3 departs from its specifications:

- US-43 expects `ip.not-for-us` for a datagram to an unjoined group in a frame to that group's MAC; the frame filter refuses it first, as `eth.not-for-us` (`wire.md` §3.3, ETH-23). Exit: the scenario names the frame filter.
- DH-64 expects an announce request from the client on link-up with a lease held; `toyos-net-ip` announces the held address itself on link-up (IP-D6, C-4), so the client asks nothing. Exit: the scenario drops the request, or [ip] stops announcing on its own.
- `udp-dhcp.md` §U9 (3) lets every datagram a closed socket had accepted leave; `toyos-net-udp` holds at most one socket's queue of them together and refuses the rest `udp.tx-discarded-on-close`, because architecture §3.3 holds nothing without a bound. Exit: the specification bounds them.

What stage 3 does not yet meet:

- Its only oracles are the readers' own: the specifications' scenarios and byte vectors, and the tests' own RFC 1071 sum. No behaviour of `toyos-net-ip`, `toyos-net-udp` or `toyos-dhcp` is checked against anything the readers did not write. Exit: exchanges captured from slirp and the T14 (ARP, DHCP, ICMP, IGMP) replay through them at stage 5 and match.

What stage 4 has not yet built: the segment-script runner, the resets owed when an address is lost (`ip.md` §8.4, each routed by its destination alone), or the remaining [net] scenarios at frame level: NET-02, NET-05 and NET-07–16 still run on `toyos-net-tcp`'s own two-node network, which goes with the last of them. Exit: each lands, or the specification moves it to a later stage.

What stage 4 departs from its specifications:

- `ip.md` §6.7 (4) and PL-12 have a waiting flow ask again at every transmit opportunity. [tcp] asks it again only once `Tcp::wake` names its peer, which [shard] calls when [ip] reports a change for that next hop (`Event::Resolved`, `Failed`, `Cleared`), for the routes, or room in a neighbour table that refused the flow for being full (`Event::Room`), so a waiting flow costs one question per change. Exit: the specification takes the wake.
- PL-14 counts `tcp.next-hop-failed` once per opportunity; the spec owner ruled once per segment not built, which [tcp] counts. Exit: PL-14 reads as the ruling.
- [ip]'s `Event::Cleared` and `Event::Room` are not in the specifications. Exit: they name them.
- A frame the sink refuses is not in the specifications: [tcp] counts it `tcp.frame-refused` and commits nothing. A reset or ACK owed outside a connection is offered again at the next `Tcp::transmit_owed`, which [shard] calls once per frame of credit, so one whose frame it refused would cost one build and one `tcp.frame-refused` per frame of credit, not one per opportunity; a connection [tcp] answers `Served::Refused` for is set aside by the round until the opportunity ends, forfeiting what is left of its turn. [shard] cannot take that path: its builder refuses a segment longer than a frame, one with options no header holds, and a source no datagram may carry, and TCP's MTU, [tcp]'s own bounds and [ip]'s addresses rule them out. Exit: the shard's builder cannot refuse, and `NotReady::Unframed`, `tcp.frame-refused` and the refused set go.
- Architecture §3.3 has a flow waiting for its next hop keep its place in the round. [shard]'s round lets it go, deficit and all, and [tcp] offers it again when woken, at the round's tail, so a waiting flow costs the round nothing. Exit: the specification takes it.
- §3.3 has a flow leave the round once it has nothing eligible. [tcp] and [udp] learn that only when served, so a flow whose turn ended on its last frame stays in the round with its deficit until its next turn, and what it queues before then is sent against that deficit, debt included. Exit: the specification takes it, or [tcp] and [udp] report at hand-off that nothing is left.
- `tcp.md` §10.8 and §11.3 disagree, and [tcp] follows §10.8: a user timeout replaces R2 and runs only while sent data is unacknowledged or a zero window holds data back, so with one set a connection whose data never left, for want of credit or of a next hop, has no give-up, where §11.3 has a device that never offers credit end its connections. Exit: the specification says which holds.

The shard keeps no timing wheel: its deadlines stay in [tcp]'s and [ip]'s ordered sets, composed through `next_deadline()` (architecture §3.2). Exit: a soak or many-flows measurement shows the ordered sets cost, and the wheel lands.

What `toyos-net-wire` does not yet meet:

- Its builders write front to back, not into prefix room ahead of a payload already in a DMA slot. Exit: netd's transmit path builds its headers into that prefix room.
- Its structured fuzz runs bounded in the PR gate only. Exit: the nightly runs it long.
- Its corpus is the specification's vectors alone, so no header layout has an oracle independent of the reader. Exit: frames captured from slirp and the T14 join the corpus.
- It departs from the wire specification: RT-06 sweeps frames and datagrams only, because a message-only vector has no length of its own to be cut against. Exit: the specification covers it.

Exit: `rg smoltcp` is empty outside `issues/`.
