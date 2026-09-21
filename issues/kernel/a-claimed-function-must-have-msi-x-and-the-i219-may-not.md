---
status: open
kind: defect
opened: 2026-09-07
---

# A claimed function must publish MSI-X, and the I219 may not

`kernel/src/pcidev/mod.rs`'s `bring_up` arms exactly one interrupt mechanism
(`:544`):

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

Why it matters: the ThinkPad T14's onboard NIC is an Intel I219 at `00:1f.6`,
`8086:15fc`, and the e1000e family's PCH parts (I217/I218/I219) are documented
as MSI parts — Linux's `e1000e` sets `FLAG_HAS_MSIX` for the 82574 and 82583
and for nothing else, so Linux would choose MSI on this family whether or not
the capability exists: under Ubuntu, `/proc/interrupts` names its interrupt
`IR-PCI-MSI-0000:00:1f.6` and `msi_irqs/162` reads `mode=msi`, and that
reading is the delivery mode Linux chose, not a read of the function's MSI-X
capability. The reading that bears on `:544` is this kernel's own, recorded
on the unmerged branch `lan-metal` (PR #442) and not in this tree's copy of
the file —
`git show 0a5717f5:issues/hardware/the-t14-answers-only-through-a-usb-stick.md`,
line 63: "`pcidev` armed MSI-X alone and refused it." So in this tree the
claim on `pci:8086:15fc` is refused `NoMsix` at `:544` before any driver
runs, netd exits, and no process on this machine can drive that cable.

**PR #442** (`lan-metal`) holds the first half of the exit condition below and
nobody holds the second. Its `kernel/src/pcidev/mod.rs` arms MSI-X first and
MSI where a function has none (`arm`,
`pci.enable_msi(vector).then_some(Armed::Msi)`), and bench boots of that
family of branches carry the hand-over — run 42 (`lancase-placement`, tip
`4204e090`), from its kernel log:

    [2026-09-14 10:09:09 1.334 cpu0] PCI 00:1f.6: msi address=0xfee000b8 data=0x00000000
    [2026-09-14 10:09:09 1.334 cpu0] pcidev: PCI 00:1f.6 [8086:15fc] handed over on slot 0, vector 0x28

What the I219 does with the IVAR write in MSI mode, measured on the bench, is
unclaimed by any task.

## The driver's MSI-X-only register on an MSI part

`toyos-i219/src/lib.rs:496-503`, whole:

```
// §10.2.4.9: `IVAR` allocates every cause to no vector at reset, so a
// part in MSI-X mode with it unprogrammed fills `ICR` and delivers
// nothing. Read back, because the document defines the register only
// "in MSI-X mode" and says nothing about what a part outside that mode
// answers — so a part that does not take the write is refused here
// rather than driven on a guess about which interrupt it would raise.
nic.regs.write(regs::IVAR, ivar::ALL_ON_VECTOR_ZERO);
nic.accepted(regs::IVAR, ivar::ALL_ON_VECTOR_ZERO)?;
```

`regs::IVAR` (`toyos-i219/src/regs.rs:28-30`) is §10.2.4.9's register, "which
'is only valid in MSI-X mode'"; the datasheet the driver cites throughout is
the 82574's, and `lib.rs:4-10` says why the I219's own is not the document.
**In this tree the write is unreachable on the T14**: `:544` refuses the
function before `open` (`lib.rs:399`) can run.

On the branches that arm MSI, the part took it. Runs 42 (`lancase-placement`,
tip `4204e090`) and 51 (`lancase-phy-control`, tip `6276fc87`) — both of
unmerged branches, both carrying the write and the readback
(`git grep -n 'regs::IVAR' 4204e090 6276fc87 -- toyos-i219/src/lib.rs`) —
record `spawn: /system/bin/netd pid=5` 0.2 s after the hand-over,
`iommu: domain6 maps 0x6800000..0x6a00000 at 0x2000000000` 0.7 s later,
`exit: test_rs_lan_hold pid=7 code=0` at 22.655 s and 24.644 s, and no
`exit: netd` record in the boot. That is an inference from an absent record:
a `Refusal` out of `open` reaches `Card::intel`
(`userland/netd/src/main.rs:87-91`), which panics through `undrivable`
(`:83-84`), and a process that dies writes `exit: <name> pid=… code=…`
(`kernel/src/process.rs:988`) — the record run 55's ring tail shows for the
same program, `exit: netd pid=5 code=-1 cpu=22ms`. So on those boots
`accepted(regs::IVAR, …)` returned `Ok` on a part in MSI mode: some word
landed where the driver looked for it. That the bits echoed is not evidence
the register routes causes the way §10.2.4.9 documents for MSI-X mode; what
`0x000E4` does on an I219 outside that mode is in neither document this
driver has read.

**Exit condition**: PR #442's MSI arm lands, so the claim is granted in this
tree; and what the I219 does with the IVAR write in MSI mode is measured on
the bench — an interrupt counted on the vector `pcidev` armed, on a boot that
leased — or the write is skipped by name where the function was armed with
MSI.
