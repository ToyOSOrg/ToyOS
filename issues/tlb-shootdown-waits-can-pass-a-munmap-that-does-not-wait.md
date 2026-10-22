---
status: open
kind: defect
opened: 2026-10-21
---

# `tlb_shootdown_waits` can pass a `munmap` that does not wait

The test's stages 1 and 2 (`tests/guest/rust-tests/src/bin/tlb_shootdown_waits.rs`) time a
`munmap` and a fixed `mmap` with `SYS_DEBUG`'s `TLB_ACK_DELAY_ARM` standing against every CPU
(`delay::EVERY`, `kernel/src/arch/x86_64/tlb.rs`). Under `EVERY` every `Asked` answer is held
20 ms, on whichever CPU gives it — the timed thread's own CPU included. So the timed CPU is held
inside `timed()` whenever it answers another CPU's shootdown there:

- by its shootdown IPI, at any point of the window — in user mode, in the syscall body (its
  interrupts are open) or nested in the timed shootdown's own wait;
- by a lock spin's `poll` before its own shootdown.

Either makes the measured call take ≥ 20 ms whether or not it waited for the flush. A kernel
whose `munmap` frees without waiting passes stage 1 if one of its five trials coincides with
another CPU's shootdown, and stage 2, a single trial, on one coincidence.

**Evidence.** Other CPUs do shoot down during this phase on the T14: the unforced
`tlb-diag:` lines of `control/` and `fixdiag-t14/` (https://github.com/ToyOSOrg/evidence/tree/2188ec6229603d845b73d3840b51b90b6b75199f) count an `unmap` issued during
the sweep's first turns (acpiserver and hda starting beside the test). No boot measured a
false pass; this is the code's reading, checked in #967's review.

The kernel cannot simply exempt "the timed CPU": after `ARM` returns it does not know which
CPU the test will time from, and the thread may migrate.

Owner: the kernel's TLB shootdown, `issues/a-tlb-shootdown-reaches-only-the-cpus-that-ran-the-space.md`'s track.

**Exit**: no answer the timed thread's CPU gives inside the timed window is held, so a
`munmap` or fixed `mmap` whose shootdown does not wait reds stages 1 and 2 even with other
CPUs shooting down; shown by a negative control that removes the wait.
