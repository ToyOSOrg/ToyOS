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

Both benches put their radio there:

- **The AMD bench**, as `main`'s kernel logged it in #884's metal run (E13 §1,
  https://github.com/ToyOSOrg/evidence/blob/2eb315326d1b4d0363b4c918da8dd2bfaabd1234/e13-amd-ax200-bar.log):
  the AX200 at 01:00.0 has BAR 0 at 0xd0600000, behind bridge 00:02.2, which
  forwards 0xd0600000..0xd0700000; no free run reaches into that range.
- **The T14**, as S1 read it (lines 111 to 116,
  https://github.com/ToyOSOrg/evidence/blob/cd368b25b2259a76b80dc31f4219dac1c91d84dc/s1/wifi-s1-a-kernel.log):
  the AX210 at 09:00.0 has BAR 0 at 0xbc300000, 16 KiB, behind root port
  00:1c.4, which forwards 0xbc300000..0xbc400000.

The Wi-Fi demo's run on the AMD bench took this path, as its commit messages
record (E13 §2): pcidev moved the AX200's BAR 0 onto eight 2 MiB windows from
0x450200000, all outside the 1 MiB that 00:02.2 forwards; every moved BAR read
0xffffffff, and the claim was refused. The demo then kept the BAR in place,
untranslated and unremapped, which `main` never does.

Two placement issues rest on placement as it is, and P1 takes them up with
this one:

- `issues/nothing-reds-when-the-bar-survey-drops-an-assigned-bar.md` asks for
  a guest that reds when the forwarded-range push is deleted. That push is
  what puts every candidate outside its bridge. Its "No machine in reach puts
  a BAR it hands over behind a bridge" holds only until a radio is handed
  over: both benches put theirs behind one.
- `issues/no-boot-reaches-the-retry-that-undoes-a-placement-nothing-answered.md`
  wants a function behind a `pcie-root-port` to settle at a later candidate.
  Under this issue no candidate lies inside a port's window, so none ever
  answers there.

Owner: P1 of `issues/there-is-no-wifi.md`.

**Exit**:
- a BAR inside every window on its path to the root is mapped in place, in
  4 KiB pages;
- a BAR that shares a 4 KiB page with another function's is moved within
  those windows, never out of them, or is refused by name;
- a host test places a BAR behind a bridge;
- the T14's AX210 is claimed with its BAR in place;
- each of the two placement issues above closed, or its exit rewritten to the
  placement P1 lands.
