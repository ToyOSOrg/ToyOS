---
status: assigned
kind: defect
opened: 2026-09-24
---

# A re-claimed PCI function spends an interrupt remapping entry it never returns

`iommu::vtd::interrupt::allocate` (`kernel/src/arch/x86_64/vtd/interrupt.rs`) hands
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

The T14 recorded it on a later boot: `irte5` for `00:1f.6` at its first claim,
and `irte6` for the same function when the swap claimed it again.

Owner: the IOMMU track's stage 1, branch `wt/toyos-iommu1`. Exit condition: a
released claim returns its entry — freed at `pcidev::release`, or one entry
kept per `pcidev` slot and rewritten on each claim, which bounds the table's
use by `MAX_FUNCTIONS` — and the `claim_reuses_its_remapping_entry` metal row
is green: the T14's I219 claimed, released and claimed again in one boot writes
one entry for both claims and leaves it not present after each release. Once
one re-claim reuses its entry, the count of claims in a boot moves nothing, so
a guest test of more than 256 is not owed; and a metal row reaches a real
function's claim before a guest test does, which is the tier root `CLAUDE.md`
puts first.
