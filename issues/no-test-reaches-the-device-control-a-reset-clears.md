---
status: open
kind: tooling
opened: 2026-09-24
---

# No test reaches the Device Control a reset clears

`kernel/src/pcidev`'s `Kept::restore` writes back Device Control, and Device
Control 2 where the Express structure has one, after a reset `release` started
on a function: Max_Payload_Size has to agree with the link partner's (PCIe
§7.5.3.4), and a function level reset returns it to its default (§6.6.2).
Nothing the harness runs can fail on it.

- QEMU's `igb`, the one function in reach that resets by an Express FLR, reads
  `devctl kept=0x0000 now=0x0000` and `devctl2 kept=Some(0) now=0x0000` across
  its release (review round 3 of #484, instrumented): firmware left both at
  their defaults, so the reset changes nothing a restore could put back.
- The mutation that deletes the restore builds, and `swap_resets_the_function`
  passes on it.

Owner: the swap's author, who added the restore. Exit condition: a boot that
releases and re-claims a function which has a reset and whose Device Control
holds a non-default Max_Payload_Size — a machine that has one, or a QEMU
function firmware configures that way — with an arm asserting the value the
next claim reads is the one the kernel read before the reset.
