---
status: open
kind: tooling
opened: 2026-10-13
---

# QEMU's `amd-iommu` writes an `IO_PAGE_FAULT` record with no address

AMD 48882 rev 3.11, Figure 56 and Table 57: an `IO_PAGE_FAULT` record
carries the device address the requester tried in its second doubleword,
and in its first `RW` at bit 53 (+04 bit 21), `PR` at 52, `I` at 51 and
`NX` at 49. QEMU 11.1.1's `amd-iommu`, refusing an `iommu-testdev` write at
`0x400000000000` under `amdvi-selftest`, wrote
`0x200a000000000030 0x0000000000000000`: the right code and DeviceID
(`00:06.0`), the address zero, and bits 51 and 49 set, which 48882 reads as
an interrupt with `NX`. Measured by logging the raw record in
`kernel/src/arch/x86_64/amdvi/fault.rs` on PR #909's branch.

So `amdvi_selftest` (`tests/common/iommu.rs`) holds the record to its
requester and its name, not its address or its access, and the record's
`addr=`/`access=` under QEMU are QEMU's, not the device's.

Owner: the orchestrator's AMD laptop track
(`issues/an-amd-vi-machine-isolates-no-device.md`); the AMD bench's
`usbd` fault row reads a real unit's record at its aimed address.

**Exit:** under the QEMU `build/ci/qemu-version` names, the record of
`amdvi-selftest`'s refused write names `0x400000000000` and a write, and
`amdvi_selftest` asserts both.
