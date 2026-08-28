---
status: open
kind: defect
opened: 2026-08-28
---

# `set_ready` publishes `SMP_READY` and `SIBLINGS_ANSWER` as two separate stores, so a shootdown between them is local-only after an AP has already joined

`kernel/src/arch/smp.rs:38-41` releases the APs and enables sibling participation in the TLB shootdown as two independent stores:

```rust
pub fn set_ready() {
    SMP_READY.store(true, Ordering::Release);      // smp.rs:39
    crate::arch::tlb::siblings_answer();           // smp.rs:40 -> tlb.rs:105-107
}
```

Nothing ties them into one event, and the order is the one that can lose a flush rather than the one that hangs.

## The mechanism

An AP spins on `SMP_READY` (`kernel/src/arch/smp.rs:268-270`) and, the moment it observes `true`, calls `tlb::join()` (smp.rs:274) — a one-shot `SHOOTDOWN.serve` (`kernel/src/arch/tlb.rs:99-102`) that reads `requested`, flushes, and publishes that generation (`kernel/src/shootdown.rs:58-62`). From then on the AP holds translations that only an issued shootdown can clear.

`shootdown()` gates on the *other* flag (`kernel/src/arch/tlb.rs:36-40`):

```rust
let cpus = smp::cpu_count();
if !SIBLINGS_ANSWER.load(Ordering::Acquire) || cpus <= 1 {
    crate::mm::paging::flush_tlb_all();
    return;
}
```

That branch never reaches `SHOOTDOWN.issue()` (tlb.rs:42, `kernel/src/shootdown.rs:52-55`), so it leaves nothing in the generation counter for `join()` (tlb.rs:99-102), `poll()` (tlb.rs:89-95) or the 0xFE vector (tlb.rs:78-84) to catch up on. `cpu_count()` is already the full SMP count here — every AP was counted at `kernel/src/arch/smp.rs:243`, long before `set_ready` — so the branch turns purely on `SIBLINGS_ANSWER`.

Both stores are `Release`, so neither the compiler nor x86-TSO can swap them: a reader can observe `SMP_READY=true` together with `SIBLINGS_ANSWER=false`, never the converse. That is the hazard direction. What the module rests on is the invariant "a shootdown issued while `SIBLINGS_ANSWER` was false is answered retroactively by every AP's `join`" — which is true only for shootdowns issued *before* an AP's join. A local-only shootdown issued *after* one is covered by nothing, and the AP keeps a stale translation for whatever page table the initiator just rewrote — SDM Vol. 3A §11.12.4 territory when the change is a memory type.

Reversing the two stores is **not** the fix: `SIBLINGS_ANSWER` first would make a shootdown in the gap `issue()` and then wait for an AP still spinning at smp.rs:268 with `IF` clear, which is the hang `kernel/src/arch/smp.rs:37` records.

## Impact today

Latent, not reachable. Two windows exist and neither has an occupant on this tree:

- **BSP**: the window is the instruction or two between smp.rs:39 and smp.rs:40. Only an asynchronous interrupt can execute there, and none of the five `tlb::shootdown()` callers — `kernel/src/process.rs:600`, `kernel/src/mm/unmapped.rs:17-22`, `kernel/src/mm/paging.rs:356`, `kernel/src/mm/paging.rs:903`, `kernel/src/arch/syscall/vm.rs:216` — is reachable from an interrupt handler; every handler body is publish-only (`kernel/src/arch/idt/xhci.rs:5-12`, `hda.rs:4-8`, `virtio_net.rs:5-12`, `virtio_sound.rs:4-8`, `dma_fault.rs:4-7` → `kernel/src/iommu/vtd/fault.rs:97-120`, `log_nest.rs:4-7`, `spurious.rs:54-59`, `nmi.rs:83-92`). The gap also cannot be widened by preemption: the ring-0 branch of the timer entry (`kernel/src/arch/idt/timer.rs:43-64`) runs no Rust and only sets `need_resched` before `iretq`, so the boot thread cannot be switched out mid-`set_ready`.
- **AP**: the window is the BSP's store-buffer drain after the AP observes `SMP_READY`. The AP's earliest shootdown-capable instruction is `drain_zero_handles` (`kernel/src/sched/driver.rs:746` → `kernel/src/object/ops.rs:154` → `kernel/src/process.rs:600`), microseconds behind `log!` (smp.rs:276) and `enter_idle_loop` (`kernel/src/sched/driver.rs:698-718`). Timing closes it, not a guard.

So the machine is correct today by accident of which code paths exist. Adding a shootdown to an IRQ handler, building the per-CPU residency masks that `issues/kernel/nothing-counts-tlb-shootdowns.md` weighs, or placing any work on the AP between smp.rs:274 and smp.rs:276 turns it into a real lost flush, and no test in the tree would see it.

## Reproduction / precondition

Not reproducible on this tree — that is the finding. The precondition is a `shootdown()` call executing on any CPU whose view of `SIBLINGS_ANSWER` is still false while another CPU has already returned from `tlb::join()`. The nearest thing to a demonstration is a third thread in `kernel-loom/tests/tlb_shootdown.rs` modelling `set_ready`'s two stores against an AP that joins and then issues, which should show the initiator taking the local-only branch with the joined CPU never flushing.

## Fix direction

Make release and participation one event: delete `SIBLINGS_ANSWER` and have `shootdown()` (tlb.rs:36) and `poll()` (tlb.rs:90) read the same word the APs are released by. Then a shootdown issued after the release always `issue()`s, and the two remaining cases are both already handled — an AP still in the spin answers through `join()`, which reads `requested` before it flushes (`kernel/src/shootdown.rs:59-61`) and so publishes a generation at least as new as the pending one, bounded by a single load rather than by `wait_for`'s 5 s tripwire (tlb.rs:66-70); an AP past `join()` answers by IPI or by `poll()`. Shootdowns before the release stay local-only and stay covered by `join()`, exactly as now.

This is a concurrency and memory-management change, so it owes its two checks: the negative control is the loom model above run against the unfixed ordering, and the independent oracle is the existing `kernel-loom` shootdown model plus its `shootdown-serve-relaxed` mutation arm (`src/build.rs:1748`), which already certifies the acknowledgement edges the fix relies on.
