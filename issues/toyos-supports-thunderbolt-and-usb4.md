---
status: open
kind: track
opened: 2026-10-13
---

# ToyOS supports Thunderbolt and USB4, PCIe tunnelled through them included

The owner wants it (2026-10-13), and nothing of it is designed yet. Until a
design is put to him and ruled, "Refuse external, record reflash" (owner,
2026-10-04) holds: a userland driver's claim of a function below an external
port is refused by name (`issues/the-iommu-refuses-nothing-yet.md`,
`issues/a-driver-that-reflashes-its-device-escapes-its-isolation.md`).

**Constraint**: root `CLAUDE.md`, Capabilities.

Owner: the orchestrator.

**Exit**: a metal row on a bench machine with a Thunderbolt or USB4 port reads,
with a device plugged into that port: the device is enumerated and driven; a
PCIe function tunnelled through it is claimed by a userland driver and its DMA
into its grants completes; and that function aimed at memory no grant maps is a
`DMA FAULT` record, the machine still running, as the row's negative control.
The owner's ruling on the design retires the external-port refusal and its row
in `issues/the-iommu-refuses-nothing-yet.md`'s exit.
