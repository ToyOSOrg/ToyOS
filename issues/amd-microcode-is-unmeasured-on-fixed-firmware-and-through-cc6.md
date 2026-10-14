---
status: open
kind: tooling
opened: 2026-10-14
---

# AMD microcode is unmeasured on fixed firmware and through CC6

The kernel's AMD loader (`kernel/src/arch/x86_64/microcode.rs`) rests, where
AMD documents nothing for Family 17h, on one run on the AMD bench, whose
firmware predates AMD-SB-7033's fix (ToyOSOrg/evidence `32c335ece`,
`metal/amd-bench/`). Two things it cannot see are unmeasured:

- **Firmware with the fix.** AMD says its fixed Family 17h server firmware
  "cannot be hot-loaded" without saying how a load fails, and says nothing
  for the bench's Renoir. The loader reports a `#GP` or an unchanged level
  as `refused`; whether such firmware refuses, and how, is unknown.
- **CC6.** The patch level is per core and resets at a warm or cold reset
  (55922 §1.4.4.11). Every CPU kept it through 2 s halted, which was not
  shown to reach CC6; AMD documents a CC6 save area for the patch on Family
  15h only. A core that lost its patch in CC6 would run firmware's level
  with nothing reading it again.

Owner: the orchestrator.

**Exit**: the AMD bench, or another Family 17h machine, on firmware with
AMD-SB-7033's fix boots the loader and its lines are recorded; and every CPU
of the bench reads its level after residency in CC6 that the bench's own
counters show, equal to the level it loaded.
