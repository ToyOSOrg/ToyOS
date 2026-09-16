---
status: open
kind: tooling
opened: 2026-09-08
---

# Nothing reaches the MSI arm of a claimed function

`pcidev::bring_up` arms a claimed function on MSI where it publishes no MSI-X,
and no test in any tier arms one. The *order* is guarded — `https_tls13_e1000e`
holds a claimed `8086:10d3`, which publishes both mechanisms, and refuses an
`msi address=` line for it — but the MSI arm itself is reached by nothing. `virtio_net_no_msix` calls
`PciDevice::enable_msi` from `bring_up` and reads false back; nothing reaches a
true, and so nothing reaches:

- `PciDevice::disable_msi` from either hand-back site (`bring_up`'s `place_bars`
  failure and `tear_down`), or `Armed::Msi`'s teardown, which turns the
  capability off where there is no table entry to mask;
- `Refusal::MsixUnusable` and `Unarmed::Blocked`, owed only by a function that
  publishes MSI-X this kernel cannot arm and by a unit that refuses the message.

The two pre-existing MSI armings in this kernel — xHCI's and HDA's
`arm_interrupt` — never disarm, so MSI teardown is exercised nowhere in the tree
at all.

Owned by the network track's stage-2 I219 worker. Exit condition: the first
`userdev` interrupt counted against a claim on `00:1f.6` on the bench, which
needs the 32-bit BAR window before it, plus netd exiting from that claim, which
runs `tear_down`'s MSI arm.

A guest arm is the alternative, and it needs no new boot config and no holder:
`kernel/src/drivers/pci.rs`'s `StagedCaps` stages a device shape on the function
an existing config already hands to a claim, and `pci_claim_caps_truncated`
reads a refusal off `tests/e1000case`. An MSI arm that *succeeds* is the same
hook with the list ending at its terminator rather than at a link the spec
forbids; what it costs from there is a registered name and its CI price, plus
whatever netd driving that card on MSI turns out to need.
