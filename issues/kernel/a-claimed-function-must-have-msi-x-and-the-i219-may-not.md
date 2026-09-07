---
status: open
kind: defect
opened: 2026-09-07
---

# A claimed PCI function must publish MSI-X, and the T14's own NIC may not

`kernel/src/pcidev/mod.rs`'s `bring_up` arms exactly one interrupt mechanism:

```
let entry = pci.enable_msix(VECTORS[slot]).ok_or(Refusal::NoMsix)?;
```

A function that publishes no MSI-X capability is refused by name, and `Bound`
holds an `Mmio` pointing at that function's one table entry so `tear_down` can
mask it. Every device this project has handed to a process so far is a virtio
function, and every one of those has MSI-X, so the refusal has never been
reached other than by `virtio_net_no_msix`'s deliberate `vectors=0`.

**The kernel can already arm the other mechanism and nothing calls it.**
`PciDevice::enable_msi` exists in `kernel/src/drivers/pci.rs`, `toyos-pci`'s
`msi` module decodes the capability, and `iommu::remap_msi` is on that path
too. What is missing is `pcidev` choosing between them and a `Bound` that can
hold either — MSI has no per-entry table, so the masking `tear_down` does has
no counterpart and the capability's optional per-vector mask bit is what
stands in for it.

Why it matters now: the ThinkPad T14's onboard NIC is an Intel I219 at
`00:1f.6`, and the e1000e family's PCH parts (I217/I218/I219) are documented as
MSI parts — Linux's `e1000e` sets `FLAG_HAS_MSIX` for the 82574 and 82583 and
for nothing else. **This has not been read off the laptop**, and it is the
first thing to check there: if `lspci -vv` on `00:1f.6` shows a `MSI-X`
capability the refusal never fires and nothing here is owed; if it shows only
`MSI`, netd's claim on `pci:8086:15fc` is refused `NoMsix`, netd exits, and
stage 2's metal half cannot run until this is built.

The driver above it is already indifferent: `toyos-i219` writes §10.2.4.9's
`IVAR` and reads it back, so a part that allocates vectors is masked in the
vectored causes' names and one that does not keeps the classic ones. What it
cannot do is get the claim in the first place.
