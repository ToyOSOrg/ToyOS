---
status: open
kind: defect
opened: 2026-09-28
---

# `SharedMemory`'s safe slices rest on a size nobody checks

`SharedMemory::as_slice`, `as_mut_slice` and `as_atomic` (`toyos/src/shm.rs`)
are safe functions that make a slice of `size` bytes over the mapping. The type
enforces neither premise that makes this sound:

- `adopt` is safe and takes `size` from its caller without checking it against
  the region the kernel mapped. A server that adopts a peer's region at the
  peer's declared length reads and writes past it in safe code.
- `shm_map` is idempotent, so `share()` followed by `adopt` gives two
  `SharedMemory` values over one mapping in one process. `as_mut_slice` on one
  then aliases `as_slice` or `as_atomic` on the other.

`#![forbid(unsafe_code)]` on the compositor (`userland/compositor/src/main.rs`)
leans on both. The compositor meets them, since it adopts only regions it
created and the kernel's framebuffer and cursor, but the compiler does not know
that.

Owner: `toyos::shm`.

**Exit**: `adopt` is an `unsafe fn` whose contract is both premises, or it
checks `size` against the kernel's region and refuses a second mapping of one
region.
