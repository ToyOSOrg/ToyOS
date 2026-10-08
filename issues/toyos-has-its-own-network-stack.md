---
status: open
kind: track
opened: 2026-09-27
---

# ToyOS has its own network stack

netd runs smoltcp. Its replacement is ToyOS's own stack, built clean-room: readers specify each stage from the RFCs, writers build from those specifications and the RFCs alone and never open another stack's source. The specifications live outside the tree; every test names its scenario id, and the review checks each scenario has one. Where a departure's exit names a test or an RFC rather than the specifications, that is the orchestrator's reading of the owner's words of 2026-10-04: "why do we need to persist prose. The specs exist we can reference them cant we?"

- Stage 0: the acceptance bar, on smoltcp.
- Stage 1: `toyos-net-wire`, in the tree.
- Stage 2: `toyos-net-tcp`.
- Stage 3: `toyos-net-ip`, `toyos-net-udp`, `toyos-dhcp`.
- Stage 4: `toyos-net-shard` and `toyos-net-testnet`.
- Stage 5: netd on one shard, pipe ABI unchanged; smoltcp leaves netd, `Cargo.toml` and `Cargo.lock` in the same PR.
- Then multi-core, netring (blocked on the owner's ABI ruling), TCP and IP hardening, IPv6, offloads, soak.

Stage 5 lands in slices, the orchestrator's cut under the owner's words "i want smoltcp out as fast as possible": netd's decisions go into `toyos-net-node` (`userland/netstack/node`), pure and host-tested and shipped in nothing, and then one change moves netd onto it and deletes smoltcp. In the tree: the node, its DHCP lease, streams, listeners and the bound on both (`userland/netstack/node/src/places.rs`). Still to build on it: datagram sockets and mDNS, the resolver; then the move.

What the node does not yet meet:

- Of what a third party wrote, only slirp's recorded OFFER and ACK have reached it (`userland/netstack/node/tests/slirp.rs`); every other frame it has answered is the tests' own, from the RFCs' layouts. Exit: netd runs on it against slirp in a guest and a router on the T14.
- It counts a DHCP message [udp] refused as `node.dhcp-unsent`, whatever the rule; the `dhcp.renew-unroutable` scenario owed above is not written. Exit: that scenario names the counter, or the node counts the renewal apart.
- With the link down the DHCP client keeps its timers: every 4 to 64 s the node has a deadline, builds a DISCOVER that [udp] refuses, and counts it in `dhcp.tx.discover` and `node.dhcp-unsent`, from `Node::new` on. Exit: `toyos-dhcp`'s client is told the link went down and waits for it, and the node's first DISCOVER is the one that leaves.
- No second TCP has been on the far end of a stream: the peer of `userland/netstack/node/tests/streams.rs` is the tests' own script, which acknowledges what arrives in order and loses, reorders and repeats nothing; what is not ours there is `etherparse`, which reads every segment the node emits and builds every one it receives. Exit: netd runs on the node against the host kernel's TCP through slirp in a guest.
- The node's places are one number for every client: a program that connects or listens in a loop takes them all, and so does a peer that keeps the connections [tcp] is finishing alive, each of which holds its stream's place (next line). The node has no word for the program behind a request, as `issues/netstack-lookup-slots-have-no-per-client-share.md` records of netd. Exit: that issue's.
- A listener's handshakes in progress are [tcp]'s `limits::LISTEN_PENDING`, each held until `limits::SYNACK_GIVE_UP`, and a SYN past them is dropped: 256 SYNs a minute from addresses that never answer shut a port to every other peer for as long as they keep coming. [tcp] has no SYN cookie and lets no older handshake go for a newer one. Exit: a test in `toyos-net-shard/tcp/tests/` in which a peer's handshake completes while LS-12's flood runs.
- The node counts a wake unspent until an accept arrives, and cannot tell an accept on its way from one that will never be sent: an owner that reads a wake and sends no accept is owed one wake fewer from then on, and the last connection waiting is not announced. That is the node's half of `issues/an-accept-that-never-reaches-netstack-strands-its-listener.md`. Exit: that issue's, by a client that cannot spend a wake without its accept arriving, or by an accept that waits in netd and needs no wake.
- A client that is gone with nothing left in its pipe is finished by [tcp]'s rules for a user who let go: a reset once it has been idle 60 s, where netd on smoltcp reset it 100 s after the client left whatever the peer said. A peer that acknowledges a byte a minute holds such a connection for as long as it has bytes to acknowledge. Exit: the T14 or a guest shows such a peer holding one, and [tcp]'s rule takes a bound from the client's leaving; or the owner rules the idle bound is the one.
- `Tcp::ready` and `Tcp::orphans` are additions to [tcp] the specifications do not hold, made for the node's wakes and places; their tests (`toyos-net-shard/tcp/tests/held.rs`) name no scenario id. Exit: the specifications hold both calls and the tests name their scenarios.
- `node.address-refused` has no test: `toyos-dhcp` accepts no address or prefix [ip] refuses, by the same `toyos-net-wire` checks in both, so the refusal cannot be reached from the wire. Exit: the client hands [ip] a type that carries the check, and the counter goes.

The listener defects are this track's: `issues/a-handshake-nobody-finishes-holds-a-listeners-port-shut.md` and `issues/a-connect-between-two-accepts-is-reset.md`, on smoltcp until stage 5, and `issues/an-accept-that-never-reaches-netstack-strands-its-listener.md`, in std's accept.

Owed from the stage 3 specifications: by stage 5, the scenario for netd's mapping of UDP's refusals onto the pipe ABI, and `dhcp.renew-unroutable`, which netd counts where `toyos-net-udp` refuses the renewal `udp.no-route`; by IP hardening, the scenarios for fragment reassembly and path MTU discovery.

What stage 3 departs from its specifications:

- US-43 expects `ip.not-for-us` for a datagram to an unjoined group in a frame to that group's MAC; the frame filter refuses it first, as `eth.not-for-us` (ETH-23). Exit: the scenario names the frame filter.
- DH-64 expects an announce request from the client on link-up with a lease held; `toyos-net-ip` announces the held address itself on link-up, by the owner's ruling to announce rather than probe again, so the client asks nothing. Exit: the scenario drops the request, or [ip] stops announcing on its own.
- The readers' UDP specification lets every datagram a closed socket had accepted leave; `toyos-net-udp` holds at most one socket's queue of them together and refuses the rest `udp.tx-discarded-on-close`, because the readers' stage 4 design holds nothing in the shard without a bound. Exit: `s_udp_us_053_accepted_datagrams_outlive_close` finds `udp.tx-discarded-on-close` at 0, every datagram a closed socket accepted leaving.

What stage 3 does not yet meet:

- Its only oracles are the readers' own: the specifications' scenarios and byte vectors, and the tests' own RFC 1071 sum. No behaviour of `toyos-net-ip`, `toyos-net-udp` or `toyos-dhcp` is checked against anything the readers did not write. Exit: exchanges captured from slirp and the T14 (ARP, DHCP, ICMP, IGMP) replay through them at stage 5 and match.

What stage 4 has not yet built: the segment-script runner, the resets owed when an address is lost, each routed by its destination alone, or the remaining [net] scenarios at frame level: NET-02, NET-05 and NET-07–16 still run on `toyos-net-tcp`'s own two-node network, which goes with the last of them. Exit: each lands, or the specification moves it to a later stage.

What stage 4 departs from its specifications:

- The readers' IP specification and PL-12 have a waiting flow ask again at every transmit opportunity. [tcp] asks it again only once `Tcp::wake` names its peer, which [shard] calls when [ip] reports a change for that next hop (`Event::Resolved`, `Failed`, `Cleared`), for the routes, or room in a neighbour table that refused the flow for being full (`Event::Room`), so a waiting flow costs one question per change. Exit: a `toyos-net-shard` test finds a waiting flow sent after each change [shard] wakes it on, `Event::Cleared` included.
- PL-14 counts `tcp.next-hop-failed` once per opportunity; the spec owner ruled once per segment not built, which [tcp] counts. Exit: PL-14 reads as the ruling.
- [ip]'s `Event::Cleared` and `Event::Room` are not in the specifications. Exit: they name them.
- The readers' stage 4 design has a flow waiting for its next hop keep its place in the round. [shard]'s round lets it go, deficit and all, and [tcp] offers it again when woken, at the round's tail, so a waiting flow costs the round nothing. Exit: a `toyos-net-shard/tests/drr.rs` test finds a woken flow served in the place it held when it began to wait.
- The readers' stage 4 design has a flow leave the round once it has nothing eligible, its deficit reset. [udp] says so as it hands out a sender's last datagram; [tcp] learns it only when it serves the connection, since only its `next_segment` decides what is due, so a connection whose turn ended on its last segment stays in the round with its deficit until its next turn, and what it queues before then is sent against that deficit, debt included, as RFC 8290 §4.2 keeps an emptied queue until it is next selected. Exit: [tcp] reports at hand-off that nothing is left, and a `toyos-net-shard/tests/drr.rs` test finds such a connection out of the round, its deficit reset.
- The readers' TCP specification disagrees with itself, and [tcp] follows its user timeout: one replaces R2 and runs only while sent data is unacknowledged or a zero window holds data back, so with one set a connection whose data never left, for want of credit or of a next hop, has no give-up, where its rule that give-up clocks run on wall time has a device that never offers credit end its connections. Exit: RFC 9293 §3.10.8 ends a connection in any state once its user timeout expires: a test that sets one and withholds credit, as `s_pl_009_give_up_without_credit` does without one, finds the connection ended.

Open for the owner. On 2026-10-03 he chose "Fair scheduler now": "Build the byte-fair scheduler as the network spec describes, about 280 lines, and decide later on the laptop whether new connections should jump the queue." It is built as one round, in the order flows became eligible, and nobody jumps it. The track reads a queue that new connections jump as RFC 8290's new-flows list, and offers him this evidence: a sparse flow such as a DNS query waits one turn of every flow ahead of it in the round, up to a quantum of 1,514 bytes each, where [udp]'s datagrams and [tcp]'s segments alternated frame by frame before, so a query waited at most one TCP frame: in DRR-02 a 100-byte segment that becomes eligible leaves behind a full frame of each of the three bulk flows ahead of it (`s_shard_drr_002_a_bulk_flow_starves_no_other_beyond_its_quantum`); a T14 measurement at stage 5 of a sparse flow's wait behind bulk flows would add to it. Exit: the owner rules on whether new connections jump the queue.

The shard keeps no timing wheel: its deadlines stay in [tcp]'s and [ip]'s ordered sets, composed through `Shard::next_deadline`. Exit: a soak or many-flows measurement shows the ordered sets cost, and the wheel lands.

What `toyos-net-wire` does not yet meet:

- Its builders write front to back, not into prefix room ahead of a payload already in a DMA slot. Exit: netd's transmit path builds its headers into that prefix room.
- Its structured fuzz runs bounded in the PR gate only. Exit: the nightly runs it long.
- Its corpus is the specification's vectors alone, so no header layout has an oracle independent of the reader. Exit: frames captured from slirp and the T14 join the corpus.
- It departs from the wire specification: RT-06 sweeps frames and datagrams only, because a message-only vector has no length of its own to be cut against. Exit: the specification covers it.

Exit: `rg smoltcp` is empty outside `issues/`.
