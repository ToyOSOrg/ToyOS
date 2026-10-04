---
status: open
kind: tooling
opened: 2026-09-13
---

# No boot reaches the refusal that keeps a BAR read inside a declared window

`kernel/src/pcidev`'s `place_bar` refuses `Refusal::BarUnrouted` when the
address firmware assigned a BAR lies inside no window firmware declared: it is
the check that keeps this kernel from issuing a load no bridge forwards, which
on real hardware does not complete. Nothing in this tree reaches it.

- q35's virtio-net answers at `0x800000000`, inside the window
  `EFI_PCI_ROOT_BRIDGE_IO_PROTOCOL::Configuration()` names, so `netcase` takes
  the other branch.
- QEMU's e1000e answers at `0xc0060000`, inside `mem 0xc0000000..0xc0100000`,
  so `e1000case` takes it too.
- The ThinkPad T14's I219 answers at `0xbcf00000`, inside
  `mem 0xa2000000..0xbd000000` (metal run 36).

So a mutation deleting those three lines is invisible to every arm the harness
can run. What *is* covered is the decision itself — `toyos_pci::aperture::decode`
is pure and tested over the T14's own windows — and the neighbouring refusal
`BarReferenceEmpty`, which `https_tls13_e1000e` reaches on that card's empty
flash BAR.

`NoRun` and `NoPlacement` are unreached for the same reason: every machine in
reach offers an address and the function answers at the first one.

**Exit condition.** A boot whose claimed function's firmware-assigned BAR lies
inside no declared window — a guest config that hands over a function QEMU puts
outside the window OVMF declares, or a machine that has one — with an arm that
asserts the `NOT HANDED OVER` record and that no read was issued.
