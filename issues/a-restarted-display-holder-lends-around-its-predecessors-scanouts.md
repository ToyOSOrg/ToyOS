---
status: open
kind: defect
opened: 2026-10-14
---

# A restarted display holder lends around its predecessor's scanouts

A virtio function on q35's root bus has no function-level reset and no power
management capability, so `pcidev`'s release cannot reset it: it keeps every
address the last holder's grants and lends were at as the function's residue
(`kernel/src/pcidev/mod.rs`, `Function::residue`), and a residue address is
never lent again (`Bound::lend_room`). A compositor started again on the same
virtio-gpu therefore lends its scanouts in `toyos_abi::pci::LEND_WINDOW` less
what the last one lent: after a holder at the largest mode, which lends two
scanouts' worth, the window has no room for a flip at that mode and it is
declined by name. QEMU keeps the old holder's resources, `max_hostmem`
included, until the next holder's own reset (§3.1.1, which
`toyos-virtio`'s `Offer::acknowledge` makes first).

The supervisor does not restart the compositor, so nothing reaches this
today.

Exit: a residue a holder's own §3.1.1 reset retires — the function's status
read 0 after the reset is the proof it holds nothing — so the window is whole
again for the next holder.
