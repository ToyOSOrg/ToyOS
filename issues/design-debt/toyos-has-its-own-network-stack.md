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

Owed from the wire specification by stages 3–5, whose layer tags are `[ip]`, `[shell]` or `[udp]`: ETH-11, 12, 22–25, 33; ARP-16–18; IP-25, 26, 35; IPP-01–13; ICMP-32–46; IGMP-23–25, 29; UDP-17, 18; and the policy halves of ETH-10, 14, 19, IP-02, 20–23, 29, IPO-15, 16, ICMP-23, 24, 26 and UDP-22.

Exit: `rg smoltcp` is empty outside `issues/`.
