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

Owed from the stage 3 specifications: OUT-07 by stage 4, whose scheduler chooses between a flow's segment and [ip]'s own frames; US-57 by stage 5, whose netd maps UDP's refusals onto the pipe ABI; FRA-01–20 and PMTU-01–06 by IP hardening.

What stage 3 departs from its specifications:

- `ip.md` §5.4 drops a datagram released from a pending queue when the 64-frame control queue is full; `toyos-net-ip` never does, because its sender was told it is held, and it stays bounded by the pending queues it was held in. Exit: the specification says so, or the owner rules the drop.
- `udp-dhcp.md` §D2 has the client count `dhcp.renew-unroutable`, but the client never learns of the refusal, which `toyos-net-udp` counts as `udp.no-route`. Exit: netd counts it where the refusal lands at stage 5, or the specification drops it.
- US-43 expects `ip.not-for-us` for a datagram to an unjoined group in a frame to that group's MAC; the frame filter refuses it first, as `eth.not-for-us` (`wire.md` §3.3, ETH-23). Exit: the scenario names the frame filter.

What `toyos-net-wire` does not yet meet:

- Its builders write front to back, not into prefix room ahead of a payload already in a DMA slot. Exit: netd's transmit path builds its headers into that prefix room.
- Its structured fuzz runs bounded in the PR gate only. Exit: the nightly runs it long.
- Its corpus is the specification's vectors alone, so no header layout has an oracle independent of the reader. Exit: frames captured from slirp and the T14 join the corpus.
- It departs from the wire specification: RT-06 sweeps frames and datagrams only, because a message-only vector has no length of its own to be cut against. Exit: the specification covers it.

Exit: `rg smoltcp` is empty outside `issues/`.
