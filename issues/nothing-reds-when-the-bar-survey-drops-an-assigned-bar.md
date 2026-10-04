---
status: open
kind: tooling
opened: 2026-09-28
---

# Nothing reds when the BAR survey drops an assigned BAR or a forwarded range

`pcidev::publish` (`kernel/src/pcidev/mod.rs`) feeds `toyos_pci::placement::free_runs`
three extents: the firmware map, every BAR firmware assigned (`taken.push` of
`memory.address()..end`), and every range a bridge forwards
(`taken.push(forwarded)`). The rule is host-tested; the two kernel pushes are
not. Deleting the BAR push still places the NIC at `0xc000200000` on QEMU's
edk2, because the 2 MiB alignment steps over firmware's BARs there, and
`alone_in_its_page` checks only BARs, not the ranges bridges forward. No
machine in reach puts a BAR it hands over behind a bridge.

Owner: orchestrator. Exit condition: a guest test goes red when either push is
deleted — a machine whose firmware places a BAR, or a bridge's forwarded range,
where the first 2 MiB-aligned candidate would otherwise land.
