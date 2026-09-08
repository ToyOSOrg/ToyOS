---
status: open
kind: tooling
opened: 2026-09-08
---

# Nothing reaches the MSI arm of a claimed function

`pcidev::bring_up` arms a claimed function on MSI where it publishes no MSI-X,
and every line of that arm is reached by no test in any tier:

- `PciDevice::enable_msi` from `bring_up`, and `PciDevice::disable_msi` from both
  hand-back sites (`bring_up`'s `place_bars` failure and `tear_down`);
- `Refusal::MsixUnusable`, which is owed only by a function that publishes MSI-X
  this kernel cannot arm;
- `Armed::Msi`'s teardown, which turns the capability off where there is no
  table entry to mask.

No device QEMU models that a process may claim publishes MSI without MSI-X, and
none publishes an MSI-X capability that cannot be armed, so no guest arm can take
either branch. `virtio_net_no_msix` reaches the neither-mechanism refusal and
nothing beyond it. The two pre-existing MSI armings in this kernel — xHCI's and
HDA's `arm_interrupt` — never disarm, so MSI teardown is exercised nowhere in
the tree at all.

On the T14, `00:1f.6` has been armed as far as the message
(`PCI 00:1f.6: msi address=0xfee000b8 data=0x00000000`, run 29) and no further:
the hand-over is refused at the BAR window, so no interrupt has ever been
delivered on MSI on any machine, and no hand-back has ever run.

Owned by the network track's stage-2 I219 worker. Exit condition: the first
`userdev` interrupt counted against a claim on `00:1f.6` on the bench, which
needs the 32-bit BAR window before it, plus netd exiting from that claim, which
runs `tear_down`'s MSI arm. A guest exit is the alternative and costs more: an
actuator that hides a function's MSI-X capability from the claim path, a boot
config whose own test binary holds a claimable function, and the tier row and CI
price of the boot that carries them.
