---
status: open
kind: defect
opened: 2026-08-22
---

# A `KernelSlice` can outlive the allocation it was built from

`kernel/src/mm/region.rs`. `KernelSlice::whole` is now the type's only
constructor and it reads the base and the size off an `mm::Allocation`, so the
size a bound is checked against is the allocation's — that half is closed. The
half that is not: `KernelSlice` is `Copy` and carries no lifetime, so a copy
taken while the `PageAlloc`/`OwnedAlloc` is alive stays nameable after it has
been dropped and its pages reissued.

`mm::Dma` is the shape that answers it: `Dma<'pool>` borrows the pool it came
out of, and `DmaPool::leak` is the only way to reach `'static`. The same
signature here would be `KernelSlice<'alloc>`.

What makes it more than a `git sed`: `elf::cache` holds a `cached_image:
KernelSlice` over pages the cache owns and hands it to every process that
clones the library, and `LoadedLib`, `TlsModule` and `UserStack` each store one
whose allocation lives in a different structure from the slice. Each of those is
a real lifetime that has to be written down rather than inferred, and getting
one wrong is a compile error rather than a silent regression — which is the
argument for doing it, and the reason it is not a small change.

No known live bug: every current holder outlives its slice today, checked by
hand when `from_raw` was deleted (2026-08-22). This is the residual, not a
reproduction.

**Promoted to `defect` 2026-08-25** (finding-lifecycle ruling; unsure resolves
to promote, and a memory-safety residual is the worst thing to lose to a wrong
fold). `KernelSlice` being `Copy` with no lifetime means nothing but hand-checking
stops a copy naming pages that have been reissued — and the hand-check was done
once, in 2026-08-22, against holders that have moved since. `mm::Dma<'pool>` is
the shape that answers it. Owed by whoever writes the four holders' lifetimes
down: `elf::cache`'s `cached_image`, `LoadedLib`, `TlsModule` and `UserStack`.
