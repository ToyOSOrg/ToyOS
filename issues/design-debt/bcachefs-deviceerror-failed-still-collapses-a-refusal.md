---
status: open
kind: finding
opened: 2026-08-29
---

# `DeviceError::Failed` can still collapse a block-layer refusal

#327 closed the `BudgetExpired -> permanent write loss` class at the level that
bit: `bcachefs::DeviceError` is a two-variant enum (`Failed`/`Refused`), so the
adapter's old `.map_err(|_| DeviceError)` erasure no longer compiles (E0423), and
every adapter site funnels a `BlockError` through the single discriminant-
preserving `From` conversion.

The class is *narrowed*, not made unrepresentable. rustc's own fix-it for the
uncompilable erasure suggests `.map_err(|_| DeviceError::Failed)`, which compiles
and re-collapses a `Refused` (a still-durable budget refusal) into the device's
own-word `Failed` path — the exact loss #327 removed. Nothing in the tree does
this today; the reviewer flagged it as a defensibility observation, not a hole.

Exit condition: make the discriminant undiscardable — e.g. `Refused` carries a
witness the `Failed` constructor cannot mint, or the `From<BlockError>`
conversion is the only reachable constructor of a `DeviceError` that names a
device operation. Then a hand-written `Failed` over a refusal stops compiling
too, and the class is unrepresentable rather than merely un-erasable.
