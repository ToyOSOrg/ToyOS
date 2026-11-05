---
status: open
kind: defect
opened: 2026-09-30
---

# The NMI entry can hand the lockup sample `cs` for `rflags`, and no test reds

`kernel/src/arch/x86_64/idt/nmi.rs` loads `note`'s one argument, the
frame's `rflags`, off the interrupt frame by offset. With that load mutated to

    "mov rdi, [rsp + {rflags_offset} - 8]",

`note` hands `hardlockup::sample` the frame's `cs` as its `flags`. Bit 9 is
clear in every selector this kernel loads, so `trap::frame_interrupts_enabled`
reads `IF` clear on every sample, and the refusal that keeps a CPU with
interrupts open out of the lockup verdict
(`moved || trap::frame_interrupts_enabled(flags)` in
`kernel/src/hardlockup/mod.rs`'s `sample`) never fires.

No test samples a CPU with `IF` set and a count that has not moved, so that
refusal is unjudged, and the metal row that staged a deaf CPU went with the
black-box page.

Nobody holds it.

**Exit**: a guest arm that samples a CPU with `IF` set and a stale count and
asserts the machine is not reset, red under the mutation above; or the entry passing
`note` a pointer to a `#[repr(C)]` five-word frame, which takes the register
order out of the entry.
