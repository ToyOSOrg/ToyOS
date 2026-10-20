---
status: open
kind: tooling
opened: 2026-09-28
---

# Nothing reds when the BAR survey drops an assigned BAR

`pcidev::publish` (`kernel/src/pcidev/mod.rs`) records every memory BAR
firmware assigned, and a claim hands a BAR's 4 KiB pages over only where no
other recorded BAR lies in them (`toyos_pci::pages::of_bar`, host-tested). The
record itself is not: deleting its push leaves every arm green, because no
machine in reach has two functions decoding in one 4 KiB page, so the check
never meets a neighbour to refuse. A survey that dropped a BAR would hand its
page to the holder of the function beside it.

Owner: orchestrator. Exit condition: a guest test goes red when the push is
deleted: a machine whose firmware places two functions' BARs in one 4 KiB page
(for one, two QEMU `i6300esb` functions, whose memory BAR is 16 bytes, if the
firmware packs them), with a row asserting the claim's `keeps BAR` refusal
names the neighbour.
