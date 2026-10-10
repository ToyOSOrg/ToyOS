---
status: open
kind: defect
opened: 2026-10-10
---

# netstack's virtio receive has no per-pass budget

`VirtioNet::poll_rx` (`userland/netstack/src/virtio_net.rs`) answers a frame for
as long as the used ring holds one, and `Node::receive`
(`userland/netstack/node/src/lib.rs`) takes frames until the card says none is
left. A device that refills the ring as fast as it is read keeps one pass in
receive: the transmit opportunity, the deadlines and every stream's pass wait
for the ring to empty, and the whole batch is read at one `now`. The I219 is
bounded at `toyos_i219::RX_BUDGET` frames a pass.

The I219's bound is not one to copy. A pass that stops at it leaves frames whose
interrupts the pass's `begin_pass` already took, and nothing in netstack's loop
(`userland/netstack/src/main.rs`) makes the next wait return at once, so they
wait for the next interrupt or deadline. Read from the code, not measured.

The fix is one bound for every driver where the pass takes its frames, at
`Node::receive`, answering that it stopped at the bound so the loop's next wait
is zero; the I219's own budget is then deleted. That touches `toyos-i219`, which
the branch that found this (#828) was not briefed to change.

**Exit condition**: a host test feeds `Node::receive` a source that never empties
and sees it stop at the bound and answer that a pass is owed; netstack's loop
waits zero after such a pass; `toyos_i219::RX_BUDGET` is gone.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
