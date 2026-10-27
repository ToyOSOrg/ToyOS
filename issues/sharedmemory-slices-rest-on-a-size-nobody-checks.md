---
status: open
kind: defect
opened: 2026-09-28
---

# `SharedMemory`'s safe slices rest on a size nobody checks

`SharedMemory::as_slice`, `as_mut_slice` and `as_atomic` (`sdk/toyos/src/shm.rs`)
are safe functions that make a slice of `size` bytes over the mapping. The type
enforces neither premise that makes this sound:

- `adopt` is safe and takes `size` from its caller without checking it against
  the region the kernel mapped. A peer's region is mapped through `lent`, which
  refuses a length the kernel's `fstat` does not answer; `adopt` is held to the
  kernel's own regions (a device window, a DMA grant, the scanout) only by its
  doc comment, so a caller that adopts a peer's region at the peer's declared
  length reads and writes past it in safe code.
- Every view aliases memory something else can write. Each `shm_map` is a
  mapping of its own at a fresh address (`sys_shm_map` maps `At::ANYWHERE`),
  and dropping one `SharedMemory` releases only its own mapping, so `share()`
  followed by `adopt` gives two values over two mappings of the same pages.
  `as_mut_slice` on one then aliases `as_slice` or `as_atomic` on the other at
  another address, and every peer holding a handle maps those pages too, so a
  slice over a region already shared is never exclusive.

`#![forbid(unsafe_code)]` on the compositor (`system/compositor/src/main.rs`)
leans on both. The compositor meets them, since it adopts only regions it
created and the kernel's framebuffer and cursor, but the compiler does not know
that.

Owner: `toyos::shm`.

**Exit**: `adopt` checks `size` against the kernel's region, and no safe
function forms a reference over bytes another mapping can write.
