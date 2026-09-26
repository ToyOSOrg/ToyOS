---
status: open
kind: defect
opened: 2026-09-26
---

# A claim spends device addresses its slot never gets back

Every `SYS_DEVICE_DMA_ALLOC` grant of a claim is placed at a fresh address of
its slot's domain (`kernel/src/pcidev/mod.rs`, `dma_alloc`), and the domain
never hands an address out twice (`kernel/src/iommu/vtd/table.rs`,
`Domain::reserve`). A holder bounded to `MAX_GRANT_TOTAL` per claim can still
claim, allocate and die over and over — a service a supervisor restarts does
exactly this — and each claim spends up to that much of the slot's addresses
and the remapping tables under them, which are never freed. The domain running
dry is a refusal (`ResourceExhausted`) and not a crash, but the slot is then
dead for the rest of the boot, and the tables are memory nothing returns.

What a holder *lends* (`SYS_DEVICE_DMA_MAP`) does not spend: it is placed in a
window reserved once per slot. Grants are not, because an address a released
function may still be aimed at must not reach a later holder's memory; the
residue mechanism answers that only for a function no reset quiets.

**Exit condition.** A slot's grant addresses are bounded as its lent ones are
— reused once the function that held them is reset and quiet, or placed in a
window reserved once — and a guest test claims, allocates and releases a
function more times than a narrowed domain (`iommu-domain-narrow`) has room
for, and the last claim is served.
