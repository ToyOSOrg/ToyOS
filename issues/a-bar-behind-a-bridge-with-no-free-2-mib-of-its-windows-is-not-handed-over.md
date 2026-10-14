---
status: open
kind: defect
opened: 2026-10-12
---

# A BAR behind a bridge with no free 2 MiB of its windows is not handed over

A claim moves each BAR it maps onto a 2 MiB window of its own
(`place_bar`, `kernel/src/pcidev/mod.rs`), because a 2 MiB page is the only
one this kernel maps into a process. Behind a bridge the window is cut only
where every bridge on the function's path forwards
(`placement::behind_bridges`, `kernel/pci/src/placement.rs`); where none of
that is free, the BAR stays where firmware put it and is handed to nobody,
named `NoRoutedRun`, and a function with no other mappable BAR is refused.
A bridge firmware sized to its BARs leaves no such 2 MiB. The BAR holding
a function's MSI-X table is placed like any other (PR #921), so it meets
the same refusal:

- **The AMD bench**: the AX200 at 01:00.0 has BAR 0 at 0xd0600000, behind
  bridge 00:02.2, which forwards 0xd0600000..0xd0700000 (#884's E13 §1,
  https://github.com/ToyOSOrg/evidence/blob/2eb315326d1b4d0363b4c918da8dd2bfaabd1234/e13-amd-ax200-bar.log);
  the xHCI at 03:00.4 is behind 00:08.1, whose 0xd0000000..0xd0500000
  bus 3 fills. Its one BAR, which holds its MSI-X table, meets
  `NoRoutedRun`, and with no other BAR the claim is refused `NoMappableBar`.
- **The T14**, as S1 read it (lines 111 to 116,
  https://github.com/ToyOSOrg/evidence/blob/cd368b25b2259a76b80dc31f4219dac1c91d84dc/s1/wifi-s1-a-kernel.log):
  the AX210 at 09:00.0 has BAR 0 at 0xbc300000, 16 KiB, its MSI-X table
  in it (`issues/there-is-no-wifi.md`, S1's read-out), behind root port
  00:1c.4, which forwards 0xbc300000..0xbc400000.

Two placement issues rest on placement as it is, and P1 takes them up with
this one:

- `issues/nothing-reds-when-the-bar-survey-drops-an-assigned-bar.md` asks for
  a guest that reds when the forwarded-range push is deleted.
- `issues/no-boot-reaches-the-retry-that-undoes-a-placement-nothing-answered.md`
  wants a function behind a `pcie-root-port` to settle at a later candidate;
  `bar_map_behind_a_bridge` now settles one at the first.

Owner: P1 of `issues/there-is-no-wifi.md`.

**Exit**:
- a BAR inside every window on its path to the root is mapped in place, in
  4 KiB pages;
- a BAR that shares a 4 KiB page with another function's is moved within
  those windows, never out of them, or is refused by name;
- the T14's AX210 is claimed with its BAR in place;
- each of the two placement issues above closed, or its exit rewritten to the
  placement P1 lands.
