---
status: open
kind: defect
opened: 2026-10-10
---

# A function the SMMUv3 does not route panics at attach

`DeviceSpace::create` and `OwnSpace::create` take no function, so on the
SMMUv3 (`kernel/src/arch/aarch64/smmu/domain.rs`) a domain is made for any
caller. `attach` then looks the function up in the unit's routes, and
`Live::stream` panics where it finds none: an enumerated function whose IORT
route names another unit, names none, or was refused
(`kernel/src/arch/aarch64/smmu/mod.rs`, `init`'s route loop). A gap in a
firmware table becomes a kernel panic at `attach`, where it is a refusal at
`create`. Nothing on QEMU's `virt` reaches it: every function there is routed
through its one unit.

**Owner.** `issues/toyos-runs-on-arm64.md`'s stage 6, whose claim through an
SMMUv3 domain is the first caller to hand `create` a function a process names.

**Exit.** `create` takes the function and refuses, by name, one the unit does
not route, before an id is spent; `attach` has no lookup that can fail; and a
guest test, or a host test of the refusal, reds with the refusal deleted.
