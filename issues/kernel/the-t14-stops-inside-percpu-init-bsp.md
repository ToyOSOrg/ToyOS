---
status: open
kind: defect
opened: 2026-09-06
---

# The T14's boot stops inside `fpu::init`

Runs 4 (`e6494990`) and 5 (`4af1cafe`, a different image — the partition GUIDs
differ) both painted the same last record and never painted another:

```
[0.000 cpu0 boot] control_regs: cpu0 cr0=0x80010033 cr4=0x00320e68 efer=0x0d01 smep smap pcid umip nx
```

Both `loader.log`s show the handoff completed, so the kernel was entered and ran
to that line. Both machines were returned by the owner's hand.

## Where

Run 4 narrowed it to `percpu::init_bsp`: `control_regs::init(0)` is called from
there, and the next record in program order was that same function's own
`percpu: BSP cpu_id=0 …`, separated only by `fpu::init`, the `wrmsr` that makes
`gs:` valid, and the `PERCPU_READY` store.

Run 5 carried a record between `fpu::init` and the `wrmsr` — `fpu::log_state`,
moved there — and **it did not print**. So the stop is inside `fpu::init`, or in
`log_state`'s own `cpuid`/`xgetbv` before its record.

That the panel would have shown it is settled: `alloc_log_shard` gives cpu0 the
boot shard itself, so the record lands in the shard the panel already renders,
and `log::emit` repaints after every record while `EARLY` holds.

## Why it is silent, which was read wrong at first

Run 4's entry here said "it hung; it did not fault, because a fault with no IDT
is a triple fault and a triple fault resets". **That is wrong**, and it was the
one thing pointing away from the answer. `ExitBootServices` does not clear
`IDTR`: the table still loaded is the firmware's, and its handlers are in
identity-mapped memory this kernel's boot map still covers. A fault there
vectors into firmware code that dead-loops — no reset, no triple fault, and
nothing on this kernel's panel. A fault and a hang look identical from the
outside, so the silence never argued for either.

`kernel/src/arch/idt` is therefore loaded partway through `init_bsp` now,
immediately after the `wrmsr` and before `control_regs::init` and `fpu::init` —
as early as it can be, because the naked stubs open on `gs:[preempt_count]` and
the `wrmsr` is what makes that address mean anything. A fault in either step now
prints its vector, registers and backtrace on the armed panel. **That is the
change that makes the next photograph name the cause**, and it costs the kernel
nothing: `main.rs` no longer calls `idt::init`, APs are untouched (the
trampoline's own `lidt` loads the same table after setting their `gs:`), and the
IDT was already the first thing `main` did after this function.

## What in `fpu::init` can do it

`CR4` in the photograph is `0x00320e68`, and **bit 18, `OSXSAVE`, is clear** —
so this kernel never executes `xsetbv`, never writes `XCR0`, never sizes an
`xsave` area, and never issues `xsave`/`xsaveopt`/`xsaves`. `UserFpState` is
`FXSAVE64` layout and says so. Tiger Lake's AVX-512 components are therefore not
reachable and cannot be what faults: the whole class the run-5 brief raised is
ruled out by one bit in the photograph.

What is left in the function is `fninit`, `ldmxcsr` of `0x1F80` (the SDM
power-on default, which sets no reserved bit), `fxsave64` into a
`repr(C, align(16))` buffer, and two comparisons. None of those has a reading
this tree can show is wrong on Tiger Lake, which is exactly why the IDT change
matters more than another guess.

One real defect was found there and fixed: `self_check`'s assertion *message*
called `percpu::cpu_id()`, which reads `gs:` — on the BSP that runs before the
`wrmsr`, so a CPU that genuinely disagreed about the default FPU state would
have faulted inside the message reporting the disagreement, and reported
nothing. `init` and `self_check` now take the id, as `log_state` already did.

**Exit condition**: one metal run whose panel shows either the `fpu:` record, or
an exception report naming the vector and the faulting instruction. The T14 is
the only judge — QEMU boots this path on every test.

Related: `issues/hardware/an-armed-tco-has-never-reset-the-t14.md` is why this
wedge needs a hand on the power button instead of resetting itself.
