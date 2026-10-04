---
status: open
kind: track
opened: 2026-10-03
---

# ToyOS has its own allocator

Ruled (owner, 2026-10-03): ToyOS writes its own allocator crate, one core
with two fronts. The kernel's front comes first: built host-first, and
swapped in for the kernel's `dlmalloc` (`kernel/src/mm/alloc.rs`) after the
three steps of `issues/kernel/toyos-beats-linuxs-latency-on-the-t14.md`.
std's front, for programs, comes later, after it is measured against the
`dlmalloc` ToyOS's std allocates with (`library/std/src/sys/alloc/toyos.rs`
in the `rust/` fork). The 2 MiB heap-growth stall is fixed now, on
`dlmalloc`, and waits for neither front.

The stall: when the kernel heap grows, `dlmalloc` calls
`KernelPageSource::alloc` (`kernel/src/mm/alloc.rs`) inside
`ALLOCATOR.dlmalloc.lock()`, and `pmm::alloc_page` (`kernel/src/mm/pmm.rs`)
scans the page bitmap and zeroes the whole 2 MiB page before it returns, so
every CPU that allocates from the heap meanwhile spins on its lock. How long
that takes on the T14 is unmeasured.

**Ruled** (owner, 2026-10-04, "Yes, that bar"): "Within 10% of mimalloc
for time and peak memory on a pinned benchmark set; faster than dlmalloc
everywhere. The benchmarks get a portable Rust runner so they run on ToyOS
itself."

The kernel's front closes
`issues/kernel/the-kernel-heap-has-none-of-slubs-hardening.md`, under the
goal the owner chose there (2026-10-04): no allocator bookkeeping inside
objects, every free checked, data apart from pointers.

Owner: the orchestrator.

**Exit**: no heap growth allocates or zeroes a 2 MiB page inside the kernel
heap's lock; the kernel allocates through ToyOS's allocator, and `dlmalloc`,
`libc`, `windows-sys` and `windows-link` are gone from `kernel/Cargo.toml` and
`kernel/Cargo.lock`, with `build::tests::the_kernel_resolves_no_libc_for_either_target`
(`src/build.rs`) deleted in the same pull request; ToyOS's std allocates
through the std front, measured against `dlmalloc` before it is swapped in;
and the ruled bar holds, read by the benchmarks' portable Rust runner, which
runs on ToyOS: the allocator within 10% of mimalloc for time and peak memory
on the pinned benchmark set, and faster than `dlmalloc` everywhere.
