---
status: open
kind: track
opened: 2026-09-29
---

# blockd drives NVMe APST and the host memory buffer

After blockd drives the T14's drive, the blockd step of
`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`. As
Linux at `Ubuntu-6.8.0-142.142`, APST stays within
`default_ps_max_latency_us` and honours `NVME_QUIRK_NO_APST` and
`NVME_QUIRK_NO_DEEPEST_PS` (`drivers/nvme/host/core.c:63,2495-2598`).

**Exit**: on the T14, the one proving machine with a physical NVMe drive, the
block figures, and the idle minute with the drive idle. **Mutation**: a host
test's table that admits a state past the bound. **Oracle**: Linux's
`nvme_configure_apst` over the T14's `id-ctrl`.
