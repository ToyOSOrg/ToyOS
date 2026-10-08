---
status: open
kind: tooling
opened: 2026-10-08
---

# `lan_talk` has not been read on the T14 since a device call began to borrow its claim's binding

#763 makes every `pcidev` call act on a `&Binding` its claim lends, and a poll
register through the claim. The I219 under traffic is the one holder that
drives those calls on real hardware at rate: its interrupt records
(`take_record`, `has_irq`), its polls and its DMA grants. The `lan_talk` metal
row is what reads it, and it was staged from #763's head and not read: the
bench's wired path between the T14 and the development machine is down.

The row cannot tell #763 from `main` while the path is down: staged from
`main` at `6f87cdb9c` and booted the same day, it is red the same way, `the
stream never opened` (`src/metaltalk.rs`), which is the host failing to reach
the machine and not the machine's answer.

What stands in for it meanwhile: under QEMU, `iommu_virtio_platform` and the
suite's network tests drive the same `pcidev` functions through virtio-net's
claim; on the T14, `claim_reuses_its_remapping_entry`, `isa_` and `acpi_`. None
is the I219.

Owner: the orchestrator, which holds the T14 and the bench.

**Exit**: `lan_talk` green from the T14 on a head that contains #763.
