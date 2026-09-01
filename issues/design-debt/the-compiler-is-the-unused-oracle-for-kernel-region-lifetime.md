---
status: open
kind: track
opened: 2026-09-01
---

# `KernelSlice` has no lifetime, and the compiler is the oracle nobody has asked

`kernel/src/mm/region.rs:1` already says it: "`KernelSlice` is `Copy`, carries
no lifetime, and can outlive its `Allocation`". The type is declared at
`region.rs:15` with a base and a size in the two fields under it, and hands out
a real slice at `region.rs:68`. Two records turn on that:

- `issues/design-debt/kernelslice-outlives-its-allocation.md` — the allocation
  can be gone.
- `issues/isolation/kernelslice-over-user-memory.md` — the allocation can be a
  page userland still writes, so a shared Rust reference claims an exclusivity
  the mapping does not give.

**The shape of the answer is already written down, in the first of those.** It
names `mm::Dma` as the precedent — `Dma<'pool>` borrows the pool it came out of,
and `DmaPool::leak` is the only way to reach `'static` — and says the same
signature here is `KernelSlice<'alloc>`. It also names who owes the work: the
four holders whose lifetimes have to be written down, `elf::cache`'s
`cached_image`, `LoadedLib`, `TlsModule` and `UserStack`. This track adds two
things that record does not carry, and nothing else.

**First: compile-fail cases.** A lifetime is only an oracle if something proves
it refuses. Add the escape and the recycle as compile-fail tests, so the
compiler's refusal is the negative control rather than a claim about it. A
runtime allocation-generation check earns its place only on a path a lifetime
provably cannot encode, and then as test instrumentation, never on a hot path.

**Second: the user-memory half needs more than a lifetime**, because the writer
is not in this program. Drive the real construction and copy primitive in a host
harness against a writer that mutates at every copy boundary, and require the
result to be either an owned copy or a word-atomic protocol. A cooperative
writer cannot reproduce a hardware race, so the harness's verdict is evidence
only where it agrees with a separate stress of the real copy path.

`src/sourcegate.rs`'s `no_slice_or_exclusive_reference_is_built_over_a_mapped_page`
is the precedent for preferring a compile-or-gate answer here: it closed the
boundary's version of this question over `toyos-abi/src/ring.rs` and
`kernel/src/user_ptr.rs`, and the defect record it closed is already deleted
from this tracker.
