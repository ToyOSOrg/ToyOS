---
status: open
kind: defect
opened: 2026-10-14
---

# A VT-d fault is named unlike the specification, or not at all

`kernel/src/arch/x86_64/vtd/fault.rs`'s `reason_name` named only the reasons where two other
implementations agreed. Against VT-d 5.20 (D51397-019) Table 30 and Table 15:

- 07h is named `paging-entry-invalid`; the specification's LSS.1 is a hardware attempt to
  access a second-stage paging entry that resulted in an error, an unreadable entry.
- 0Ch (LSS.2, reserved field set in a second-stage paging entry), 0Dh (LCT.5, translation type
  blocking translation requests), 0Eh (LGN.4, output address in the interrupt range) and the
  interrupt-remapping reasons from 27h are printed `unnamed`.

Owner: the IOMMU work in `kernel/src/arch/x86_64/vtd/`.

**Exit**: every reason the unit can report in the mode the kernel programs is named by
the condition the specification gives it, and a host test holds each code to its name.
