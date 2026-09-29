---
status: open
kind: track
opened: 2026-09-29
---

# blockd drives NVMe APST, and a host memory buffer confined to its domain

After blockd drives the T14's drive, the blockd step of
`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`. As
Linux at `Ubuntu-6.8.0-142.142`, APST stays within
`default_ps_max_latency_us` and honours `NVME_QUIRK_NO_APST` and
`NVME_QUIRK_NO_DEEPEST_PS` (`drivers/nvme/host/core.c:63,2495-2598`). A drive
whose HMPRE is non-zero gets a host memory buffer sized as Linux at v6.8 sizes
it: HMPRE capped at `max_host_mem_size_mb`, 128 MiB, and none when HMMIN is
past that cap (`drivers/nvme/host/pci.c:55,2019-2037`). The buffer is mapped in
blockd's IOMMU domain and in no other, since the controller writes it at will.

**Exit**: host tests of the APST table and of the buffer's size over HMPRE and
HMMIN; on the T14, the one proving machine with a physical NVMe drive, the
block figures, the idle minute with the drive idle, and, where its `id-ctrl`
reads HMPRE non-zero, the IOMMU's tables read back holding the buffer in
blockd's domain alone; where it reads zero, the host tests alone prove the
buffer. **Mutation**, each red: a table that admits a state past the bound; a
buffer past the cap; the buffer mapped in a second domain. **Oracle**: Linux's
`nvme_configure_apst` and `nvme_setup_host_mem` over the T14's `id-ctrl`.
