---
status: open
kind: tooling
opened: 2026-10-23
---

# No readback has judged the NVMe driver on a bench's own disk

Every image's `nvme` row drives NVMe by its class code, `pci:01:08:02`, so a
bench boot drives the internal disk its host boots from: the T14's
`1c5c:1327` at 04:00.0 and the AMD bench's `144d:a808` at 02:00.0, as their
kernels list them. What keeps that host's partitions unwritten is held on the
host and under QEMU: a role's grant names a partition the loader named or a
ToyOS type (`toyos_supervisor::storage`), and a TOYOS-DATA partition that holds
no volume of ours and no designation stamp is never written
(`a_host_disk_beside_the_stick_is_left_alone`, on QEMU's controller under
Intel's id).

Every bench boot now runs on it: the benches boot their images off their
internal NVMe, so the log and boot volumes are served through it. No
readback has judged it yet. The controller's bring-up, its geometry (`MDTS` of at
least 32 blocks, 512- or 4096-byte sectors), its shutdown at the stop, and
the host booting again off a controller ToyOS drove are each unmeasured on
both benches.

**Exit**: the `nvme_leaves_the_benchs_disk_alone` row passes on the T14 and on
the AMD bench, each readback taken after the metal loop booted the host again
off that disk.

**Owner**: the orchestrator, which runs metal.
