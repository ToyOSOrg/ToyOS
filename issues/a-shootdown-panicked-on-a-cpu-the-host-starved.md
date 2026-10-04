---
status: open
kind: defect
opened: 2026-09-24
---

# A TLB shootdown panicked the kernel on a vCPU the host had starved

One fast-tier `cargo test` on the dev host, run beside ten `lan_swap` runs one
after another, went red on `tlb_shootdown_cost`, and it was green alone:

```
[kernel 5.599 cpu0] PANIC: panicked at src/arch/tlb.rs:171:42:
tlb: cpu 2 has not flushed for generation Generation(23) in 5000000000ns — it is not taking interrupts
    kernel::arch::tlb::shootdown+0x369
    kernel::arch::tlb::bench+0x94
```

Every guest here is TCG, and a vCPU thread the host does not schedule for five
seconds takes no interrupts for five seconds. The bound reads a starved vCPU as
a dead CPU and ends the machine. A kernel that panics over its host's schedule
crashes on any oversubscribed hypervisor. It is also the class of red that gets
re-run away.

The owed decision is whether this wait is a correctness bound, where a CPU
that never answers is a fault worth a panic, or a liveness guard that should
widen with the harness's measured host speed the way the suite's other
ceilings do.
