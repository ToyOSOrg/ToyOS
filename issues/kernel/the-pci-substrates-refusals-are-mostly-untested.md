---
status: open
kind: defect
opened: 2026-09-07
---

# Most of the PCI substrate's refusals have no arm anywhere

`kernel/src/pcidev/mod.rs` refuses a claim seven ways and refuses a call four
more, and only some of those refusals are read back by anything.

What is checked today:

- `Refusal::NoMsix` — `virtio_net_no_msix`.
- `Refusal::Untranslated` — `iommu_virtio_platform`'s no-unit arm.
- The register-window bound (past the end, straddling it, misaligned) — netd's
  own `config_space_is_bounded` at bring-up, read back by
  `iommu_virtio_platform`; the wrapping offset is host-tested in `toyos-dma`.
- A device address outside a grant — `userdev_dma_fault`.

What is not:

- `ClaimError::{Ambiguous, KernelDriven, Exhausted, Owned}`.
- `Refusal::{NoReset, NoWindow, BarUnsizable, BarUnplaceable, Dead}`.
- `SYS_DEVICE_BAR_MAP` on an index that names no memory BAR, and on the BAR
  holding the MSI-X table or PBA — **the ABI's headline safety claim**: the
  mechanism is `bring_up` leaving `bar_bytes == 0` for that index and
  `bar_object` refusing on it, and nothing reads that back.
- `SYS_DEVICE_DMA_ALLOC` with `bytes == 0`, over `MAX_GRANT_BYTES`, and past
  `MAX_GRANT_TOTAL`.
- `SYS_DEVICE_REG_WRITE` on a `PciFunction` claim answering `NotSupported`.
- Any of the three substrate calls handed a claim of another class, or a handle
  that is not a claim at all.

**Why it stands.** The only claimable function on the test machine is the NIC's,
and netd holds it, so a guest binary that could ask these questions has nothing
to ask them of. Every one of them needs either a second claimable function on
the machine or a boot config in which a test binary holds the NIC's claim
instead of netd.

**Exit condition.** A boot config whose `[programs]` gives a test binary
`devices = ["pci:1af4:1041"]` in place of netd, and one registration over it
that walks the list above; plus arms for the `ClaimError`s, which need a second
program declaring the same function (`Owned`), a config naming a kernel-driven
function (`KernelDriven`), and a machine created with two of one card
(`Ambiguous`). `Exhausted` needs five claimable functions and is the one that
may stay unreachable.

Found by the review of the network track's stage 1 (`SEND BACK`, finding 14);
recorded rather than fixed because the fix is a boot config and a registration
of its own, not a line in the substrate.
