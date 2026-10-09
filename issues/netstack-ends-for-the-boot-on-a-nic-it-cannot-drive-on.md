---
status: open
kind: defect
opened: 2026-10-09
---

# netstack ends for the boot on a NIC it cannot drive on

`system.toml` gives `restart = true` to diskserver and fileserver and not to
netstack. netstack ends by name, "this NIC cannot be driven on", wherever a
card stops being one it can drive: a claim that refuses its interrupt read, an
Intel part that kept stranded descriptors past the driver's deadline
(`Card::begin_pass`, `userland/netstack/src/card.rs`), and a virtio device
whose used ring is not believed (`finished`,
`userland/netstack/src/virtio_net.rs`), which is any of a head past the table,
a head with no chain in flight, a length past a chain's writable bytes and a
used index past the available one. After any of them the machine has no
network until it boots again, and every program holding the `netstack` port
holds a port nobody serves.

Ending is the right answer to the device: none of the four is something a
conforming device writes, and a ring read on past one loses frames silently.
What is missing is what comes after the end.

A restart is not one line in `system.toml`. A second netstack on the same
address meets `issues/a-replacement-netstack-reuses-its-predecessors-ports.md`,
every claim it mints spends device addresses
(`issues/a-claim-spends-device-addresses-its-slot-never-gets-back.md`), and
section 3 of `issues/the-lan-is-not-yet-production-grade.md` splits the driver
from the stack so that a driver's end does not take the stack's connections.

Owned by whoever takes that section. Exit: a guest test ends netstack's driver
and reads the network served again without a boot, or the owner rules that a
NIC whose device broke its ring stays down for the boot and this file becomes
that ruling.
