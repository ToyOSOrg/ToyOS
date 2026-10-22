---
status: open
kind: defect
opened: 2026-09-26
---

# A claim spends device addresses its slot never gets back

Every `SYS_DEVICE_DMA_ALLOC` grant of a claim is placed at a fresh address of
its slot's domain (`kernel/src/pcidev/mod.rs`, `dma_alloc`), and the domain
never hands an address out twice (`kernel/pure/iova.rs`, `Window::reserve`,
through `kernel/src/iommu/domains.rs`). A holder bounded to `MAX_GRANT_TOTAL`
per claim can still claim, allocate and die over and over — a service a
supervisor restarts does exactly this — and each claim spends up to that much of
the slot's addresses and the remapping tables under them, which are never freed.
The domain running dry is a refusal (`ResourceExhausted`) and not a crash, but
the slot is then dead for the rest of the boot, and the tables are memory
nothing returns. Nor is running dry always a refusal: every table under a fresh
address comes from `Tables::alloc` (`kernel/src/arch/x86_64/vtd/table.rs`),
whose `expect("no physical memory for a remapping table")` panics the kernel
when physical memory runs out first, and no second-level table a claim made is
ever freed.

What a holder *lends* (`SYS_DEVICE_DMA_MAP`) does not spend: it is placed in a
window reserved once per slot. Grants are not, because an address a released
function may still be aimed at must not reach a later holder's memory; the
residue mechanism answers that only for a function no reset quiets.

**Exit condition.** A slot's grant addresses are bounded as its lent ones are
— reused once the function that held them is reset and quiet, or placed in a
window reserved once — and a test claims, allocates and releases a
function more times than its domain has room for, and the last claim is
served. With no narrowed domain that is about 262,144 2 MiB grants on a 39-bit
unit, so a host test over `kernel/pure/iova.rs` and
`kernel/src/iommu/domains.rs` is the place to hold it; the
`iommu-domain-narrow` actuator, which narrowed a domain for this, was removed
in `29e100612`. And the tables under a slot's addresses are
bounded or freed once nothing maps through them, and a remapping table the
machine has no memory for is a refusal of the grant that needed it, not a
panic.
