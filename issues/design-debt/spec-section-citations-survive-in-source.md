---
status: open
kind: defect
opened: 2026-08-25
---

# `§N.N` citations in source point into a spec corpus the tree deleted

The spec documents were deleted by owner ruling; the deleting merge did not
sweep the source comments that cite their section numbers, so `§2.3a`, `§6.4`,
`§16.1` and kin survive as pointers at nothing — the exact rot the tracker's
deletion law exists to prevent, and no gate catches it because the marks live
in prose. The same corpus is also cited by bare label — `I5`, `C6`, `C12`,
`RT4`, `B10`, "invariant 10", "rejection 3" — and those are the same defect
wearing a different mark.

**Not every `§` is dead.** A citation into a live external document — Intel
SDM, xHCI 1.2, virtio 1.2, USB 2.0, the HDA specification, the PCIe base spec,
fatgen103 — is a boundary contract and stays. Only the marks into the deleted
internal corpus are the rot, and telling them apart is the work: an external
citation names its document, and an internal one says "spec §N", "§N's rule",
or cites a measurement no specification could carry.

The fix per site is the prose sweep's third wave's: say what the section said
in one clause, derived from what the surrounding code enforces, or delete the
sentence that leaned on it — never a bare unmarking, which keeps the sentence
and loses its ground.

## What is left

`rg -n '§[0-9]' $(git ls-files '*.rs')` is the finder: **555 lines across 137
files**, measured 2026-08-25 after the fourth prose wave. That total mixes live
external citations with dead ones, so it is a search bound and never a score.

By area:

- **`kernel/src`** — 112 lines in 41 files. Waves 3 and 4 resolved theirs; the
  remainder is the twenty files two concurrent sweeps hold (`vfs.rs`,
  `inbox.rs`, `main.rs`, `trace.rs`, `time.rs`, `mm/paging.rs`,
  `arch/entry.rs`, `arch/idt/mod.rs`, `arch/syscall/machine.rs`,
  `arch/syscall/proc.rs`, `sched/payload.rs`, `object/device.rs`,
  `object/shm.rs`, `iommu/vtd/table.rs`, `iommu/vtd/fault.rs`,
  `drivers/nvme.rs`, `drivers/i8042/mod.rs`, `drivers/panic_console/mod.rs`,
  `drivers/xhci/wait/msc.rs`, `drivers/xhci/wait/boot.rs`) plus the external
  citations that stay.
- **`toyos-sched/`** — 260 lines, the largest concentration in the tree and
  almost entirely the deleted scheduler-core spec: module headers open with
  "spec §9.2", and the sim and loom crates cite it by section throughout.
- **the rest** — `toyos-xhci/`, `toyos-hda/`, `toyos-mixer/`, `userland/`,
  `tests/` and `src/` carry the balance, a mixture of both kinds.
