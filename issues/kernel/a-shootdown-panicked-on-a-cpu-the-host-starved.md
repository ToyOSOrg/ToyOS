---
status: expected-red
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

## `quiesce_wakes_on_the_last_exit` is disabled on this file

PR #536's review of 7e2e1043 records it `EXIT=1 wide, green alone`: a TLB
shootdown panicked on a vCPU that took no interrupt for 5 s, and fsd's sync
missed its 5 s in the same boot. That review's next round attributes the red
on that branch to its own stop-time file syncs instead. On main's lineage the
name was red with another sentence, `the stopped-boot drain carried no kernel
output at all (38 bytes)`, at f231c43e (run 36280285913, `guest (3)`, green on
its re-run), 67a430c8 (run 36285169430) and a55d62c6 (run 36287592139), with
no shootdown panic in those jobs' logs; that is not shown to be this defect.

**Exit condition.** Re-enabled when the decision above is made and taken —
the wait either widens with the measured host speed or stays a correctness
bound that a starved vCPU is shown not to reach — and a
`quiesce_wakes_on_the_last_exit` red is attributed or no longer occurs beside
other guests. Owner: the shootdown's wait in `kernel/src/arch/x86_64/tlb.rs`;
nobody is holding it yet.
