---
status: open
kind: defect
opened: 2026-09-26
---

# The Intel driver drops what its transmit ring cannot hold

`i219::Nic::tx` (`userland/netd/src/i219.rs`) writes a frame the driver has
no transmit slot for into a scratch buffer and drops it, and `Card::tx_room`
answers yes for the Intel card whatever its sixteen-slot ring
(`toyos_i219::TX_RING`) holds. So smoltcp keeps handing it frames past a full
ring, and each one past is a loss this machine made itself: the peer learns of
it only as a gap, and the sender only by timing out or by duplicate
acknowledgements.

The virtio driver had the same shape and no longer does: a frame with no slot
free waits in `DmaNic`'s backlog (`userland/netd/src/main.rs`), and the ring's
completion interrupt is netd's wake; the backlog serves any card whose
`Card::tx_room` answers truthfully. Measured there before that change, on netcase with
QEMU's virtio-net: three rounds of a 32 MiB download and a 32 MiB upload in
one boot made netd's report count 770 frames dropped with no transmit
descriptor free; after it, three boots of the same counted none.

Not measured on the Intel parts: a QEMU `e1000e` run and the T14 are both
owed. The change there needs a way to ask `toyos_i219::Driver` for room without
counting a drop (`tx_reserve` counts one), and a transmit-descriptor
write-back interrupt the driver can wait on.

Exit condition: an upload through the Intel driver drops no frame of netd's
own making, with the upload test run on `e1000e`.
