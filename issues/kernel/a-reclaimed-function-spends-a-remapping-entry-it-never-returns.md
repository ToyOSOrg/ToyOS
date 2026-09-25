---
status: open
kind: defect
opened: 2026-09-24
---

# A re-claimed PCI function spends an interrupt remapping entry it never returns

`iommu::vtd::interrupt::allocate` (`kernel/src/iommu/vtd/interrupt.rs`) hands
out the next entry of a 256-entry table (`ENTRIES`) by bumping `used`, and no
path gives one back. Every `pcidev` claim arms MSI or MSI-X through
`interrupt::msi`, so every claim of a function takes a new entry, and its
release keeps it.

A boot claims each function once, so this was a constant cost until the
service swap (`toyos-swap`) made re-claiming a function routine: each swap
releases a service's claims and mints them again for the replacement. The
review of the swap's pull request (#484) read, on the T14's run 123 log, the
highest entry at `irte5` after boot and `irte6` after one swap; by its reading
of the code, a swap that goes into service costs one entry per claimed
function, and a failed one that restarts the old binary costs two.

**What it costs when it runs out:** once `used` reaches 256, every MSI claim
on the machine is refused (`TableFull`, which `pcidev` reports as
`NoInterrupt`), so a swap then ends with the service `gone` — and so does
every later claim of any function, by any process.

Owner: the swap's author, because the swap is what turned a constant into a
leak. Exit condition: a released claim returns its entry — freed at
`pcidev::release`, or one entry kept per `pcidev` slot and rewritten on each
claim, which bounds the table's use by `MAX_FUNCTIONS` — and a guest test that
claims and releases one function more than 256 times in a boot stays armed.
