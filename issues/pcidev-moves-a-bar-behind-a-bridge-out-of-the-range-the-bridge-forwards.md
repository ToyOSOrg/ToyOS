---
status: open
kind: defect
opened: 2026-10-12
---

# pcidev moves a BAR behind a bridge out of the range the bridge forwards

Before a hand-over, `kernel/src/pcidev/mod.rs` re-places each BAR onto a
2 MiB boundary of its own (`place_bar`). The candidates come from
`placement::reserve` (`kernel/pci/src/placement.rs`), and a candidate is one
of the free runs: a gap left by everything the machine decodes, inside a
root bridge's window.

The range every bridge forwards to its secondary bus is part of that
"everything" (the `forwarded()` loop that feeds `placement::free_runs`). So no
candidate ever lies inside the range a function's own bridge forwards. A
function on a secondary bus decodes at none of the `MAX_CANDIDATES` (8)
addresses it is offered, and its claim is refused.

Measured on the AMD bench, by the Wi-Fi demo's run (#876 §1, from its log):

- the AX200 has one BAR, BAR 0, 16 KiB, with its MSI-X table at +0x2000;
- it sits behind a bridge that forwards 1 MiB;
- pcidev moved the BAR onto eight 2 MiB windows outside that range, the
  function answered at none of them, and the claim was refused.

The demo then kept the BAR in place, untranslated and unremapped, which
`main` never does.

Owner: P1 of `issues/there-is-no-wifi.md`.

**Exit**:
- a BAR inside every window on its path to the root is mapped in place, in
  4 KiB pages;
- a BAR that shares a 4 KiB page with another function's is moved within
  those windows, never out of them, or is refused by name;
- a host test places a BAR behind a bridge;
- the T14's AX210 is claimed with its BAR in place.
