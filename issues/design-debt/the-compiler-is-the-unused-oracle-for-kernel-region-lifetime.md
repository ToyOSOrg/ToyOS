---
status: open
kind: track
opened: 2026-09-01
---

# `KernelSlice` has no lifetime, and the strongest available oracle is the compiler nobody asked

`kernel/src/mm/region.rs:1` already says it: "`KernelSlice` is `Copy`, carries
no lifetime, and can outlive its `Allocation`". The type carries a base and a
size (`region.rs:15`) and hands out a real slice at `region.rs:68`. Two records
turn on that:

- `issues/design-debt/kernelslice-outlives-its-allocation.md` — the allocation
  can be gone.
- `issues/isolation/kernelslice-over-user-memory.md` — the allocation can be a
  page userland still writes, so a shared Rust reference claims an exclusivity
  the mapping does not give.

**What to build, in this order, and the order is the point.** Give `KernelSlice`
an allocation lifetime and add compile-fail cases for the escape and the recycle.
That makes the *compiler* the oracle, which is the tree's own stated preference —
unrepresentable over checked over tested. Only for the paths a lifetime provably
cannot encode does a runtime allocation-generation check earn its place, and then
only as test instrumentation, never on a hot path.

**The user-memory half needs more than a lifetime**, because the writer is not in
this program. Drive the real construction and copy primitive in a host harness
against a writer that mutates at every copy boundary, and require the result to
be either an owned copy or a word-atomic protocol. A cooperative writer cannot
reproduce a hardware race, so the harness's verdict is only evidence where it
agrees with a separate stress of the real copy path.

**What this replaces.** The earlier prescription was a generation model first.
That is the weaker instrument: a generation check can be written, pass, and leave
the type still able to express the bug. `src/sourcegate.rs`'s
`no_slice_or_exclusive_reference_is_built_over_a_mapped_page` is the precedent —
a compile-or-gate answer over `toyos-abi/src/ring.rs` and `kernel/src/user_ptr.rs`
closed that boundary's version of this question, and the defect record it closed
is already deleted from this tracker.
