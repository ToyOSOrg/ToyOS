---
status: open
kind: finding
opened: 2026-08-01
---

# `#[alloc_error_handler]` does not exist anywhere in the kernel

`git grep alloc_error_handler -- kernel/` returns nothing, so a failed kernel
heap allocation takes `alloc`'s no_std default: `handle_alloc_error`
(`rust/library/alloc/src/alloc.rs:629`) reaches `__rdl_alloc_error_handler`
(`:658`), which is `panic_nounwind_fmt` (`:659`) into the kernel's own
`#[panic_handler]`.

**The premise this entry carried is refuted, measured.** It said heap exhaustion
"routes into `try_recover_from_panic`, the path that frees nothing — so the
terminal state of every unbounded-growth entry in this file is an OOM that
cannot report itself cleanly". A throwaway boot actuator asking a
`Vec::with_capacity` for `MAX_HEAP_ALLOC` bytes at 4096-byte alignment — the
shape `KernelPageSource::alloc` refuses, taken through `Vec` so the null reaches
`RawVec` rather than `debug_heap_alloc`'s checked return — printed, with no
thread current:

    [kernel 0.278 cpu0] PANIC: panicked at library/alloc/src/alloc.rs:659:9:
    memory allocation of 2093056 bytes failed
    [kernel 0.278 cpu0]   Backtrace:
    [kernel 0.279 cpu0]     0xffff80007cfbb85d  core::panicking::panic_nounwind_fmt+0x2d
    [kernel 0.279 cpu0]     0xffff80007cfb7109  __rustc::__rdl_alloc_error_handler+0x39
    [kernel 0.279 cpu0]     0xffff80007cfb71c1  alloc::alloc::handle_alloc_error+0x13
    [kernel 0.279 cpu0]     0xffff80007cfb71d6  alloc::raw_vec::handle_error+0x15
    [kernel 0.280 cpu0]     0xffff80007cf631d7  <alloc::vec::Vec<kernel::kernel_main::Page>>::with_capacity+0x57

and said nothing further in the two seconds the harness drained before killing
the guest. The size, the layer and the call site are all named, so the report is
clean; the halt after it was not observed, only `kernel/src/main.rs:178`'s
`apic::halt_all_cpus()` read. `try_recover_from_panic` is not even on that path:
with no thread current `main.rs:170`'s `recoverable` is false. In syscall context
`recoverable` is true and the recovery does run — but only after `crash_report`
(`:154`) and `panic_flush` (`:167`) have already put the same report out, and
the poisoned thread's process is zombified and reaped, which drops its whole
`ProcessData`.

**What remains, and neither part is this entry's.** The failure kills whichever
thread happened to allocate, not the one that exhausted the heap — that is
`issues/kernel/nothing-charges-kernel-memory-to-a-process.md`. And nothing can
fail an *arbitrary* allocation at an *arbitrary* site, so no path but this one
has ever been walked — that is
`issues/kernel/only-one-allocation-can-be-made-to-fail-and-only-at-one-site.md`,
which also owns the actuator the measurement above used and did not keep.
