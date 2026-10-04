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
path reaches, and `tests/virtsmpcase` starts no ACPI server. Owner: none yet.

`main` at `d47b383cf`, `cargo test -- virt_mask_windows`, three runs, each
EXIT=0: alone at load 45 rising to 72 (PASS, 10 s); beside the branch's own
whole guest suite at load 74 to 77 (PASS, 38 s); and again beside it at load
75 to 67 (PASS, 7 s). The 38 s run printed nothing between its image built
and its first guest line for 31 s, and paid its liveness ceilings at 8.00x
(the others 3.99x and 2.62x). Not reproduced on `main`, and not ruled out
there.

**Exit**: `virt_mask_windows` run at least as often as it took to see this,
alongside a load like that one, on `main`, reads no stall; or the stall's
cause, named and fixed.
