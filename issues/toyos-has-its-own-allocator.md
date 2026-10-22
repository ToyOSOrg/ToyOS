---
status: open
kind: track
opened: 2026-10-03
---

# ToyOS has its own allocator

Ruled (owner, 2026-10-03): ToyOS writes its own allocator crate, one core
with two fronts. The kernel's front comes first: built host-first, and
swapped in for the kernel's `dlmalloc` (`kernel/src/mm/alloc.rs`) after the
three steps of `issues/toyos-beats-linuxs-latency-on-the-t14.md`.
std's front, for programs, comes later, after it is measured against the
`dlmalloc` ToyOS's std allocates with (`sdk/std/sys/alloc.rs`). The 2 MiB heap-growth stall is fixed now, on
`dlmalloc`, and waits for neither front.

**Owner reaffirmed, 2026-10-22:** prioritize the backing-memory migration
(4 KiB ordinary pages, noncontiguous physical backing and lazy population
under the agreed commitment contract) and eliminate unbounded userland heap
spinning before evaluating the replacement heap's performance. Carry the lock
fix in the direct migration where practical; no temporary allocator or bridge
is required. Preserve ordinary Rust allocation contracts. The replacement
remains ToyOS-owned, with the kernel and std fronts above.

**Ruled** (owner, 2026-10-04, "Yes, that bar"): "Within 10% of mimalloc
for time and peak memory on a pinned benchmark set; faster than dlmalloc
everywhere. The benchmarks get a portable Rust runner so they run on ToyOS
itself."

The kernel's front closes
`issues/the-kernel-heap-has-none-of-slubs-hardening.md`, under the
goal the owner chose there (2026-10-04): no allocator bookkeeping inside
objects, every free checked, data apart from pointers.

Owner: the orchestrator.

**Exit**: no heap growth allocates or zeroes a 2 MiB page inside the kernel
heap's lock; the kernel allocates through ToyOS's allocator, and `dlmalloc`,
`libc`, `windows-sys` and `windows-link` are gone from `kernel/Cargo.toml` and
from the kernel's graph in `Cargo.lock`, with `build::tests::the_kernel_resolves_no_libc_for_either_target`
(`build/build.rs`) deleted in the same pull request; ToyOS's std allocates
through the std front, measured against `dlmalloc` before it is swapped in;
and the ruled bar holds, read by the benchmarks' portable Rust runner, which
runs on ToyOS: the allocator within 10% of mimalloc for time and peak memory
on the pinned benchmark set, and faster than `dlmalloc` everywhere.
