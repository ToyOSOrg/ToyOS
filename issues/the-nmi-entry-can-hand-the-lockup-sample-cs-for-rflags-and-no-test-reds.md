---
status: open
kind: defect
opened: 2026-09-30
---

# The NMI entry can hand the lockup sample `cs` for `rflags`, and no test reds

`kernel/src/arch/x86_64/idt/nmi.rs` loads `note`'s three arguments off the
interrupt frame one register at a time. With the `rflags` load mutated to

    "mov rdx, [rsp + {rip_offset} + 8]",

`note` hands `hardlockup::sample` the frame's `cs` as its `flags`. Bit 9 is
clear in every selector this kernel loads, so `trap::frame_interrupts_enabled`
reads `IF` clear on every sample, and the refusal that keeps a CPU with
interrupts open out of the lockup verdict
(`moved || trap::frame_interrupts_enabled(flags)` in
`kernel/src/hardlockup/mod.rs`'s `sample`) never fires.

No test samples a CPU with `IF` set and a count that has not moved, so that
refusal is unjudged. `hard_lockup_ends_a_deaf_cpu` stages a CPU with `IF`
clear, where the mutant and the right answer agree, and its `sp=0xffff`
assertion catches the `rsi`/`rdx` swap, not this. `main` had the same gap with
`rcx`, before the entry stopped loading `cs`.

Nobody holds it.

**Exit**: a guest arm that samples a CPU with `IF` set and a stale count and
asserts nothing is sealed, red under the mutation above; or the entry passing
`note` a pointer to a `#[repr(C)]` five-word frame, which takes the register
order out of the entry.
