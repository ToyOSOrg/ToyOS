---
status: open
kind: defect
opened: 2026-10-10
---

# The SMMUv3's global errors are read and never delivered

`kernel/src/arch/aarch64/smmu/` reads `SMMU_GERROR` against `SMMU_GERRORN`
only where something else brings it there: in every bounded wait on a register
or the command queue, and after each drain of the event queue. Its own wired
interrupt, the IORT's `GERR` GSIV, is routed nowhere and
`SMMU_IRQ_CTRL.GERROR_IRQEN` stays clear, so an error the unit raises between
those reads goes unseen until the next one, and one it raises on a quiet
machine is never seen (IHI 0070 H.a §7.5 lists them):

- `SFM_ERR`: the unit entered Service Failure Mode, terminating every client
  transaction and no longer accessing its queues (§12.3).
- `EVENTQ_ABT_ERR`: a write to the event queue aborted, and events were lost
  with no record of them; read only after a drain, which a lost record does
  not start.
- `CMDQ_ERR` outside a wait: none can arise there today, since every command
  is issued under a wait for its `CMD_SYNC`.

**Owner.** `issues/toyos-runs-on-arm64.md`'s stage 6, its ITS work: it edits
`kernel/src/arch/aarch64/irqchip.rs` and `trap.rs`, where a second SPI is
routed and taken.

**Exit.** `SMMU_IRQ_CTRL.GERROR_IRQEN` is set and the `GERR` SPI reaches a
handler that names each active error and acknowledges it in `SMMU_GERRORN`;
`virt_smmu`, or a test beside it, raises one — a `CMD_SYNC` behind an illegal
command is QEMU's `CMDQ_ERR` — and reds with the routing deleted.
