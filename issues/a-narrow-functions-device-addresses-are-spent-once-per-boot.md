---
status: open
kind: finding
opened: 2026-10-21
---

# A narrow function's device addresses are spent once per boot

A domain hands each device address out once, for the boot
(`kernel/pure/iova.rs`), and a function keeps its domain across claims. So
the addresses below a function's width are a per-boot budget. On the AMD
bench a 32-bit function has 1,704 leaves of 2 MiB below 4 GiB, the lend
window's 128 among them. A claim may hold up to 16 leaves of grants, and
retired grant addresses are never reused. Past roughly a hundred claims of
one narrow function in a boot, its next grant is refused (`ResourceExhausted`).
That refusal is clean, but nothing else announces the limit.

Exit: either the budget is shown unreachable for every narrow function a
shipped driver claims, or an address whose function's reset has completed is
handed out again, which the residue rules would have to allow.
