---
status: open
kind: tooling
opened: 2026-09-08
---

# Nothing reaches the MSI arm of a claimed function

`pcidev::bring_up` arms a claimed function on MSI where it publishes no MSI-X,
and no test in any tier arms one. `virtio_net_no_msix` calls
`PciDevice::enable_msi` from `bring_up` and reads false back; nothing reaches a
true, and so nothing reaches:

- `PciDevice::disable_msi` from either hand-back site (`bring_up`'s `place_bars`
  failure and `tear_down`), or `Armed::Msi`'s teardown, which turns the
  capability off where there is no table entry to mask;
- `Refusal::MsixUnusable` and `Unarmed::Blocked`, owed only by a function that
  publishes MSI-X this kernel cannot arm and by a unit that refuses the message;
- `Refusal::CapsTruncated`, the arm that turns a list ending at a link the spec
  forbids into a refusal of the hand-over. The decision under it is read back:
  `CapWalk::truncated` is host-tested in `toyos-pci/src/caps.rs` and
  `PciDevice::capability`'s `Truncated`/`Absent` split is driven over a cyclic
  list, a misaligned link, a below-header link and a forbidden head by the
  `pci-cap-selftest` actuator, which `pci_capability_walk` reads as
  `pci cap split 5/5`. What no test reaches is `bring_up` refusing on it.

The two pre-existing MSI armings in this kernel — xHCI's and HDA's
`arm_interrupt` — never disarm, so MSI teardown is exercised nowhere in the tree
at all.

Owned by the network track's stage-2 I219 worker. Exit condition: the first
`userdev` interrupt counted against a claim on `00:1f.6` on the bench, which
needs the 32-bit BAR window before it, plus netd exiting from that claim, which
runs `tear_down`'s MSI arm.

A guest arm is the alternative and costs more than the bench does. It needs a
claimable function that publishes MSI and no MSI-X, which nothing this harness
hands to a claim is: the NIC under `vectors=0` publishes neither, so hiding a
capability is not enough by itself. So it costs a boot config carrying such a
function and a test binary to hold it, plus that boot's tier row and CI price.
