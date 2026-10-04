---
status: open
kind: defect
opened: 2026-10-04
---

# `virt_mask_windows` went quiet inside a whole suite on a loaded host

In the whole guest suite at `wt/toyos-acpi1`'s `b9430ed61`, `virt_mask_windows`
(AArch64, the `mask-windows` kernel, `tests/virtsmpcase`, eight vCPUs under
TCG) ended `STALLED: waiting for the boot's last word — it went quiet` after
45 s. That run's workers spent 19038 s building against 1375 s testing, and
the host's load average read 82.65 / 83.62 / 77.89 moments after it, on 14
cores. The same test alone at that load, at the same commit, passed in 14 s.

Nothing in that branch runs on that boot: its AArch64 changes are stubs no
path reaches, and `tests/virtsmpcase` starts no ACPI server. Whether `main`
goes quiet the same way under that load has not been read. Owner: none yet.

**Exit**: `virt_mask_windows` run at least as often as it took to see this,
alongside a load like that one, on `main`, reads no stall; or the stall's
cause, named and fixed.
